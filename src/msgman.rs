

use std::collections::{HashMap, VecDeque};
// use std::sync::{Mutex, Arc};
use std::sync::Arc;

use chrono::Utc;
use serenity::model::prelude::{Message, ChannelId, UserId};
use serenity::futures::StreamExt;
use serenity::prelude::*;
use sqlx::{Pool, Sqlite, FromRow};

pub type ChannelQueue = Arc<Mutex<CappedQueue>>;

pub struct CappedQueue {
    queue: VecDeque<Message>,
    limit: usize
}

#[derive(Default)]
pub struct MessageManager {
    pub channel_queues: HashMap<ChannelId, ChannelQueue>,
    pub database: Option<Pool<Sqlite>>
}

#[derive(FromRow)]
struct ChannelLimitDatabaseEntry {
    channel_id: String,
    channel_limit: u32
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
        println!("Initializing {} queues from database", query_result.len());
        for line in query_result {
            if let Ok(chn) = line.channel_id.parse::<u64>() {
                match self.update_limit(http, &ChannelId::from(chn), line.channel_limit as usize, true, None).await {
                    Ok(expl) => println!("{}", expl),
                    Err(expl) => eprintln!("{}", expl)
                }
            } else {
                eprintln!("Unparseable channel id in database: {}", line.channel_id);
            }
        }
        println!("Finished initializing queues from database");

        self.database = Some(database);

    }

    pub async fn insert_message(&self, ctx: &Context, msg: Message, push_back: bool) {
        let Some(mux) = self.channel_queues.get(&msg.channel_id) else {return};
        let cq_ref = mux.clone();
        let mut cq = cq_ref.lock().await;

        // If queue is already full, remove the oldest message and delete it
        while cq.queue.len() >= cq.limit {
            if let Some(old_message) = cq.queue.pop_front() {
                if let Err(error) = old_message.delete(ctx).await {
                    eprintln!("Failed to delete message: {}", error);
                }
            } else {
                eprintln!("Queue is full but failed to pop message");
            }
        }
        if push_back {
            cq.queue.push_back(msg);
        } else {
            cq.queue.push_front(msg);
        }
    }

    pub async fn remove_limit(&mut self, channel: &ChannelId, user_id: UserId) -> Result<String, String> {
        if let Some(db) = self.database.as_ref() {
            let _result_limit = sqlx::query("DELETE FROM channel_limits WHERE channel_id=?").bind(channel.to_string()).execute(db).await.unwrap();

            let _result_audit = sqlx::query("INSERT INTO channel_limit_edits VALUES (?,?,?,?)")
                .bind(user_id.to_string())
                .bind(channel.to_string())
                .bind(0 as u32)
                .bind(Utc::now().timestamp_millis())
                .execute(db).await.unwrap();
            // println!("DB update affected {:?} rows", result.rows_affected());
            return Ok(format!("Removed limit for channel <#{}>!", channel));
        } else {
            return Err("Database is not initialized".to_string());
        }
    }

    pub async fn update_limit(&mut self, ctx: &Context, channel: &ChannelId, new_limit: usize, is_init: bool, user_id: Option<UserId>) -> Result<String, String> {
        let Some(mux) = self.channel_queues.get(channel) else {
            // We do not have a queue for this channel yet, so create it
            let new_queue = Arc::new(Mutex::new(CappedQueue { queue: VecDeque::with_capacity(new_limit), limit: new_limit}));
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
                                eprintln!("Failed to delete message: {}", error);
                            }
                        }
                    },
                    Err(error) => {
                        eprintln!("Uh oh! Error: {}", error);
                        return Err(error.to_string())
                    },
                };
                message_count = message_count + 1;
            }

            if !is_init {
                if let Some(db) = self.database.as_ref() {
                    let _result_limit = sqlx::query("INSERT INTO channel_limits VALUES (?, ?)")
                        .bind(channel.to_string())
                        .bind(new_limit as u32)
                        .execute(db).await.unwrap();

                    let _result_audit = sqlx::query("INSERT INTO channel_limit_edits VALUES (?,?,?,?)")
                        .bind(user_id.expect("Limit updated but no user received").to_string())
                        .bind(channel.to_string())
                        .bind(new_limit as u32)
                        .bind(Utc::now().timestamp_millis())
                        .execute(db).await.unwrap();
                    // println!("DB update affected {:?} rows", result.rows_affected());
                    return Ok(format!("Created limit {} for channel <#{}>, and I'm already purging older messages!", new_limit, channel));
                } else {
                    return Err("Database is not initialized".to_string());
                }
            } else {
                return Ok(format!("Initialized channel {} limit to {}", channel, new_limit));
            }
        };

        if let Some(db) = self.database.as_ref() {
            let _result_limit = sqlx::query("UPDATE channel_limits SET channel_limit=? WHERE channel_id=?")
                .bind(new_limit as u32)
                .bind(channel.to_string())
                .execute(db).await.unwrap();

            let _result_audit = sqlx::query("INSERT INTO channel_limit_edits VALUES (?,?,?,?)")
                .bind(user_id.expect("Limit updated but no user received").to_string())
                .bind(channel.to_string())
                .bind(new_limit as u32)
                .bind(Utc::now().timestamp_millis())
                .execute(db).await.unwrap();
            // println!("DB update affected {:?} rows", result.rows_affected());
        } else {
            return Err("Database is not initialized".to_string());
        }

        let cq_ref = mux.clone();
        let mut cq = cq_ref.lock().await;
        let old_limit = cq.limit;
        let old_capacity = cq.queue.capacity();

        // Edge case, but we can early return here
        if old_limit == new_limit {return Ok(format!("{} already is the limit for <#{}>!", new_limit, channel))};

        if old_limit < new_limit {
            // Capacity is increasing, just update it (not like we can recover deleted messages anyway)
            println!("Increase capacity (alloc diff = {})", new_limit - old_capacity);
            if new_limit > old_capacity {
                cq.queue.reserve(new_limit - old_capacity);
            }
            cq.limit = new_limit;
            Ok(format!("Okay, I increased the limit of <#{}> from {} to {}!", channel, old_limit, new_limit))
        } else {
            // Capacity is decreasing, so we need to purge (old_limit - new_limit) messages from the queue
            let mut remaining_messages = old_limit - new_limit;
            while remaining_messages > 0 {
                if let Some(old_message) = cq.queue.pop_front() {
                    if let Err(error) = old_message.delete(ctx).await {
                        eprintln!("Failed to delete message: {}", error);
                    }
                } else {
                    eprintln!("Queue is full but failed to pop message");
                }
                remaining_messages = remaining_messages - 1;
            }
            println!("Cut capacity down -> now is {} (should be {})", cq.queue.len(), new_limit);
            Ok(format!("Okay, I decreased the limit of <#{}> from {} to {}, and I'm already purging older messages!", channel, old_limit, new_limit))
        }
    }
}