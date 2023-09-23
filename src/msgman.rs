

use std::collections::{HashMap, VecDeque};

use chrono::Utc;
use serenity::model::prelude::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::{Message, ChannelId, UserId, MessageId, GuildId};
use serenity::futures::StreamExt;
use serenity::prelude::*;
use sqlx::{Pool, Sqlite, FromRow};
use string_builder::Builder;
use tokio::sync::mpsc::Receiver;
use log::{debug, error, warn, info};

const CHANNEL_PIN_LIMIT: usize = 50;

pub enum Command {
    Initialize {
        context: Context,
    },
    MessageReceived {
        context: Context,
        message: Message,
    },
    MessageDeleted {
        context: Context,
        channel_id: ChannelId,
        message_id: MessageId,
        guild_id: Option<GuildId>,
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
    GetStatus {
        context: Context,
        interaction: ApplicationCommandInteraction,
    },
    ChannelPinsUpdated {
        context: Context,
        channel: ChannelId,
    },
}

#[derive(Clone)]
pub struct CappedQueue {
    queue: VecDeque<Message>,
    pins: VecDeque<Message>,
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
                    MessageDeleted { context, channel_id, message_id, guild_id: _ } => {message_manager.remove_message(&context, message_id, &channel_id);},
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
                    GetStatus { context, interaction } =>
                        {
                            let content = message_manager.get_status();
                            reply_deferred(&interaction, &context, content, true).await;
                        },
                    ChannelPinsUpdated { context, channel } => {message_manager.on_pins_updated(&context, channel).await;},
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

    pub async fn on_pins_updated(&mut self, ctx: &Context, channel: ChannelId) {
        let Some(cq) = self.channel_queues.get_mut(&channel) else { return; };
        let Ok(updated_pins) = channel.pins(ctx).await else { return; };
        // updated_pins.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

        let mut added_pins = VecDeque::with_capacity(CHANNEL_PIN_LIMIT);
        let mut removed_pins = Vec::with_capacity(CHANNEL_PIN_LIMIT);

        // First we check for known pins missing from the channel
        for existing_pin in cq.pins.iter() {
            if !updated_pins.iter().any(|channel_pin| channel_pin.id == existing_pin.id) {
                // If the updated pin list does not contain the known `existing_pin` then it was removed
                removed_pins.push(existing_pin.clone());
            }
        }
        debug!("Removed {} pins", removed_pins.len());

        // Then we check for new pins missing from the queue
        for channel_pin in updated_pins.iter() {
            if !cq.pins.iter().any(|existing_pin| channel_pin.id == existing_pin.id) {
                // If the local pin list does not contain the new `channel_pin` then it was added
                added_pins.push_back(channel_pin.id);
            }
        }
        debug!("Added {} pins", added_pins.len());

        // We remove the newly-added pins (a.k.a. we retain the non-newly-added pins)
        cq.queue.retain(|message| !added_pins.contains(&message.id));

        // We re-add the newly-removed pins, sort them, and discard the excess
        for message in cq.queue.drain(..) {
            removed_pins.push(message);
        }
        debug!("Temporary queue has {} messages (limit={})", removed_pins.len(), cq.limit);
        removed_pins.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        
        // Move it back from temporary Vec
        cq.queue = VecDeque::from(removed_pins);
        while cq.queue.len() > cq.limit {
            if let Some(old_message) = cq.queue.pop_front() {
                debug!("on_pins_updated: Popping and deleting last message (id={}; ts={}) (now {} vs {})", old_message.id, old_message.timestamp, cq.queue.len(), cq.limit);
                if let Err(error) = old_message.delete(ctx).await {
                    error!("Failed to delete message: {}", error);
                }
            } else {
                error!("Queue is full but failed to pop message");
            }
        }

        cq.pins = updated_pins.into();
        debug!("Local pins list now has {} items", cq.pins.len());
    }

    pub async fn insert_message(&mut self, ctx: &Context, msg: Message, push_back: bool) {
        let Some(cq) = self.channel_queues.get_mut(&msg.channel_id) else {return};

        // If queue is already full, remove the oldest message and delete it
        while cq.queue.len() >= cq.limit {
            if let Some(old_message) = cq.queue.pop_front() {
                debug!("insert_message: Popping and deleting last message (now {} vs {})", cq.queue.len(), cq.limit);
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

    pub fn remove_message(&mut self, _ctx: &Context, msg_id: MessageId, channel_id: &ChannelId) {
        let Some(cq) = self.channel_queues.get_mut(channel_id) else {return};
        cq.queue.retain(|message| message.id != msg_id);
        cq.pins.retain(|message| message.id != msg_id);
        debug!("Queue after remove_message len={}", cq.queue.len());
        debug!("Pins after remove_message len={}", cq.pins.len());
    }

    pub fn insert_pin(&mut self, _ctx: &Context, msg: Message) {
        let Some(cq) = self.channel_queues.get_mut(&msg.channel_id) else {return};

        if cq.pins.is_empty() {
            // Simply insert it
            debug!("Insert new pin {} (channel={}; ts={}) at the front", msg.id, msg.channel_id, msg.timestamp);
            cq.pins.push_front(msg);
            return;
        }

        // Insert in the correct chronological position
        let mut index = 0;
        for queued_msg in cq.pins.iter() {
            if queued_msg.timestamp > msg.timestamp {
                debug!("Insert new pin {} (channel={}; ts={}) at idx={} (was msg {}; ts={})", msg.id, msg.channel_id, msg.timestamp, index, queued_msg.id, queued_msg.timestamp);
                cq.pins.insert(index, msg);
                return;
            }
            index += 1;
        }
        
        // If its still not added, we can assume it is the newest
        debug!("Insert new pin {} (channel={}; ts={}) at the back (now len={})", msg.id, msg.channel_id, msg.timestamp, cq.pins.len());
        cq.pins.push_back(msg);
    }

    pub fn get_status(&self) -> String {
        let mut builder = Builder::default();
        if self.channel_queues.len() > 0 {
            builder.append("The following channels are being autodeleted:\n");
            for (channel, cq) in self.channel_queues.iter() {
                let usage = (cq.queue.len() as f64) / (cq.limit as f64);
                builder.append(format!("- {} | {} / {} ({:.0}% full)\n", channel.mention(), cq.queue.len(), cq.limit, usage * 100.0));
            }
        } else {
            builder.append("There are no channels being autodeleted");
        }
        builder.string().unwrap()
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
            let new_queue = CappedQueue { queue: VecDeque::with_capacity(new_limit), pins: VecDeque::with_capacity(CHANNEL_PIN_LIMIT), limit: new_limit};
            self.channel_queues.insert(*channel, new_queue);
            
            // Now iterate over the channel's messages and delete as needed
            let mut all_messages = channel.messages_iter(ctx).boxed();
            let mut message_count = 0;

            while let Some(message_result) = all_messages.next().await {
                match message_result {
                    Ok(msg) => {
                        if msg.pinned { 
                            // Skip pinned messages (they are handled separately)
                            continue;
                        }
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

            match channel.pins(ctx).await {
                Ok(pinned_messages) => {
                    for pinned_message in pinned_messages {
                        self.insert_pin(ctx, pinned_message);
                    }
                },
                Err(error) => {
                    error!("Uh oh! Error: {}", error);
                },
            };

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
                    debug!("update_limit: Popping and deleting last message (now {} vs {})", queue.queue.len(), queue.limit);
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