

use std::collections::{HashMap, VecDeque};

use chrono::Utc;
use serenity::model::prelude::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::{Message, ChannelId, UserId};
use serenity::futures::StreamExt;
use serenity::prelude::*;
use sqlx::{Pool, Sqlite, FromRow};
use tokio::sync::mpsc::Receiver;
use log::{debug, error, warn, info};

pub enum Command {
    Initialize {
        context: Context,
    },
    MessageReceived {
        context: Context,
        message: Message,
    },
    SetLimit {
        limit: usize,
        context: Context,
        interaction: ApplicationCommandInteraction,
    },
    RemoveLimit {
        context: Context,
        interaction: ApplicationCommandInteraction,
    },
}

pub struct CappedQueue {
    queue: VecDeque<Message>,
    limit: usize,
}

#[derive(Default)]
struct MessageManager {
    initialized: bool,
    channel_queues: HashMap<ChannelId, CappedQueue>,
    database: Option<Pool<Sqlite>>,
}

pub struct MessageManagerReceiver {}

#[derive(FromRow)]
struct ChannelLimitDatabaseEntry {
    channel_id: String,
    channel_limit: u32
}

impl MessageManagerReceiver {
    pub fn run(&self, mut receiver: Receiver<Command>) {
        async fn reply_deferred(interaction:&ApplicationCommandInteraction, context: &Context, content: String, _ephemeral: bool) {
            if let Err(why) = interaction
            .create_followup_message(context, |response| {
                response
                .content(content)
            }).await
            {
                warn!("Cannot respond to slash command: {}", why);
            }
        }

        let _manager = tokio::spawn(async move {
            let mut message_manager: MessageManager = MessageManager {..Default::default()};
            
            // Start receiving messages
            while let Some(cmd) = receiver.recv().await {
                use Command::*;
                match cmd {
                    Initialize { context } => {message_manager.init(&context).await;}
                    MessageReceived { context, message } => {message_manager.insert_message(&context, message, true).await;},
                    SetLimit { limit, context, interaction } => 
                        {
                            let content = message_manager.update_limit(&context, &interaction.channel_id, limit, false, Some(interaction.user.id)).await;
                            reply_deferred(&interaction, &context, content, true).await;
                        },
                    RemoveLimit { context, interaction } => 
                        {
                            let content = message_manager.remove_limit(&interaction.channel_id, interaction.user.id).await;
                            reply_deferred(&interaction, &context, content, true).await;
                        },
                }
            }
        });
    }
}

impl MessageManager {
    pub async fn init(&mut self, http: &Context) {
        // Initiate a connection to the database file, creating the file if required.
        let database = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(5)
                .connect_with(
                    sqlx::sqlite::SqliteConnectOptions::new()
                        .filename("./database/database.sqlite")
                        .create_if_missing(true),
                )
                .await
                .expect("Couldn't connect to database");
        
        // Run migrations, which updates the database's schema to the latest version.
        sqlx::migrate!("./migrations").run(&database).await.expect("Couldn't run database migrations");

        let query_result = sqlx::query_as::<_, ChannelLimitDatabaseEntry>("SELECT * FROM channel_limits").fetch_all(&database).await.unwrap();
        debug!("Initializing {} queues from database", query_result.len());
        for line in query_result {
            if let Ok(chn) = line.channel_id.parse::<u64>() {
                let init_result = self.update_limit(http, &ChannelId::from(chn), line.channel_limit as usize, true, None).await;
                debug!("{}", init_result);
            } else {
                error!("Unparseable channel id in database: {}", line.channel_id);
            }
        }
        info!("Finished initializing queues from database");

        self.database = Some(database);
        self.initialized = true;

    }

    pub async fn insert_message(&mut self, ctx: &Context, msg: Message, push_back: bool) {
        let Some(cq) = self.channel_queues.get_mut(&msg.channel_id) else {return};

        // If queue is already full, remove the oldest message and delete it
        while cq.queue.len() >= cq.limit {
            if let Some(old_message) = cq.queue.pop_front() {
                debug!("Popping and deleting last message (now {} vs {})", cq.queue.len(), cq.limit);
                if let Err(error) = old_message.delete(ctx).await {
                    error!("Failed to delete message: {}", error);
                }
            } else {
                error!("Queue is full but failed to pop message");
            }
        }
        if push_back {
            cq.queue.push_back(msg);
        } else {
            cq.queue.push_front(msg);
        }
        debug!("Pushed new message (now {} vs {})", cq.queue.len(), cq.limit);
    }

    pub async fn remove_limit(&mut self, channel: &ChannelId, user_id: UserId) -> String {
        match self.channel_queues.remove(channel) {
            Some(mut old_cq) => {
                old_cq.queue.clear();
                if let Some(db) = self.database.as_ref() {
                    let _result_limit = sqlx::query("DELETE FROM channel_limits WHERE channel_id=?").bind(channel.to_string()).execute(db).await.unwrap();
                    debug!("DB update affected {:?} rows", _result_limit.rows_affected());

                    let _result_audit = sqlx::query("INSERT INTO channel_limit_edits VALUES (?,?,?,?)")
                        .bind(user_id.to_string())
                        .bind(channel.to_string())
                        .bind(0 as u32)
                        .bind(Utc::now().timestamp_millis())
                        .execute(db).await.unwrap();
                    debug!("DB update affected {:?} rows", _result_audit.rows_affected());
                } else {
                    error!("Database is not initialized");
                }
                format!("Removed limit ({}) from <#{}>", old_cq.limit, channel)
            }
            None => format!("<#{}> doesn't have a limit!", channel)
        }
    }

    pub async fn update_limit(&mut self, ctx: &Context, channel: &ChannelId, new_limit: usize, is_init: bool, user_id: Option<UserId>) -> String {
        
        async fn update_db(channel: &ChannelId, new_limit: usize, user_id: Option<UserId>, db_ref: Option<&Pool<Sqlite>>) -> Result<(), ()> {
            if let Some(db) = db_ref {
                let _result_limit = sqlx::query("INSERT OR REPLACE INTO channel_limits VALUES (?, ?)")
                    .bind(channel.to_string())
                    .bind(new_limit as u32)
                    .execute(db).await.unwrap();
                debug!("DB update affected {:?} rows", _result_limit.rows_affected());

                let _result_audit = sqlx::query("INSERT INTO channel_limit_edits VALUES (?,?,?,?)")
                    .bind(user_id.expect("Limit updated but no user received").to_string())
                    .bind(channel.to_string())
                    .bind(new_limit as u32)
                    .bind(Utc::now().timestamp_millis())
                    .execute(db).await.unwrap();
                debug!("DB update affected {:?} rows", _result_audit.rows_affected());
                Ok(())
            } else {
                error!("Database is not initialized");
                Err(())
            }
        }
        let Some(queue) = self.channel_queues.get_mut(channel) else {
            // We do not have a queue for this channel yet, so create it
            let new_queue = CappedQueue { queue: VecDeque::with_capacity(new_limit), limit: new_limit};
            self.channel_queues.insert(*channel, new_queue);
            
            // Now iterate over the channel's messages and delete as needed
            let mut all_messages = channel.messages_iter(ctx).boxed();
            let mut message_count = 0;

            while let Some(message_result) = all_messages.next().await {
                match message_result {
                    Ok(msg) => {
                        if message_count < new_limit {
                            self.insert_message(ctx, msg, false).await
                        } else {
                            // We can already delete older messages
                            if let Err(error) = msg.delete(ctx).await {
                                error!("Failed to delete message: {}", error);
                            }
                        }
                    },
                    Err(error) => {
                        error!("Uh oh! Error: {}", error);
                        return error.to_string()
                    },
                };
                message_count = message_count + 1;
            }
            debug!("Sanity set queue limit to {} (message_count={})", new_limit, message_count);

            if !is_init {
                let _ = update_db(channel, new_limit, user_id, self.database.as_ref()).await;
                return format!("Created limit {} for channel <#{}>, and I'm already purging older messages!", new_limit, channel);
            } else {
                return format!("Initialized channel {} limit to {}", channel, new_limit);
            }
        };

        let _ = update_db(channel, new_limit, user_id, self.database.as_ref()).await;

        let old_limit = queue.limit;
        let old_capacity = queue.queue.capacity();

        // Edge case, but we can early return here
        if old_limit == new_limit {return format!("{} already is the limit for <#{}>!", new_limit, channel)};

        if old_limit < new_limit {
            // Capacity is increasing, just update it (not like we can recover deleted messages anyway)
            debug!("Increase capacity (alloc diff = {})", new_limit - old_capacity);
            if new_limit > old_capacity {
                queue.queue.reserve(new_limit - old_capacity);
            }
            queue.limit = new_limit;
            format!("Okay, I increased the limit of <#{}> from {} to {}!", channel, old_limit, new_limit)
        } else {
            // Capacity is decreasing, so we need to purge (old_limit - new_limit) messages from the queue
            let mut remaining_messages = if queue.queue.len() > new_limit {queue.queue.len() - new_limit} else {0};
            debug!("Have to delete {} messages", remaining_messages);
            while remaining_messages > 0 {
                if let Some(old_message) = queue.queue.pop_front() {
                    debug!("Popping and deleting last message (now {} vs {})", queue.queue.len(), queue.limit);
                    if let Err(error) = old_message.delete(ctx).await {
                        error!("Failed to delete message: {}", error);
                    }
                } else {
                    error!("Queue is full but failed to pop message");
                }
                remaining_messages = remaining_messages - 1;
            }
            queue.limit = new_limit;
            debug!("Cut capacity down -> now is {} (should be {})", queue.queue.len(), new_limit);
            format!("Okay, I decreased the limit of <#{}> from {} to {}, and I'm already purging older messages!", channel, old_limit, new_limit)
        }
    }
}