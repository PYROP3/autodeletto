

use std::collections::{HashMap, VecDeque};
// use std::sync::{Mutex, Arc};
use std::sync::Arc;

use serenity::model::prelude::{Message, ChannelId};
use serenity::futures::StreamExt;
use serenity::prelude::*;

pub type ChannelQueue = Arc<Mutex<CappedQueue>>;

pub struct CappedQueue {
    queue: VecDeque<Message>,
    limit: usize
}

pub struct MessageManager {
    pub channel_queues: HashMap<ChannelId, ChannelQueue>
}

impl MessageManager {
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

    pub async fn update_limit(&mut self, ctx: &Context, channel: &ChannelId, new_limit: usize) {
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
                    Err(error) => eprintln!("Uh oh! Error: {}", error),
                };
                message_count = message_count + 1;
            }
            return
        };

        let cq_ref = mux.clone();
        let mut cq = cq_ref.lock().await;
        let old_limit = cq.limit;
        let old_capacity = cq.queue.capacity();

        // Edge case, but we can early return here
        if old_limit == new_limit {return};

        if old_limit < new_limit {
            // Capacity is increasing, just update it (not like we can recover deleted messages anyway)
            println!("Increase capacity (alloc diff = {})", new_limit - old_capacity);
            if new_limit > old_capacity {
                cq.queue.reserve(new_limit - old_capacity);
            }
            cq.limit = new_limit;
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
        }
    }
}