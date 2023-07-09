mod commands;
use std::collections::HashMap;
use std::env;

use dotenv::dotenv;

use lazy_static::lazy_static;
use serenity::async_trait;
use serenity::model::application::interaction::{Interaction, InteractionResponseType};
use serenity::model::gateway::Ready;
use serenity::model::id::GuildId;
use serenity::model::prelude::{Message};
use serenity::prelude::*;

mod msgman;
use msgman::MessageManager;

struct Bot {}

lazy_static! {
    static ref MSGMAN: Mutex<MessageManager> = Mutex::new( MessageManager {channel_queues: HashMap::new()} );
}

#[async_trait]
impl EventHandler for Bot {
    async fn message(&self, ctx: Context, msg: Message) {
        MSGMAN.lock().await.insert_message(&ctx, msg, true).await;
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::ApplicationCommand(command) = interaction {
            println!("Received command interaction: {:#?}", command);

            let content = match command.data.name.as_str() {
                "configure" => match commands::configure::run(&command.data.options) {
                    Err(_) => "Please choose a valid number".to_string(),
                    Ok(new_limit) => {
                        MSGMAN.lock().await.update_limit(&ctx, &command.channel_id, new_limit as usize).await;
                        format!("The limit for {} is {}", command.channel_id, new_limit)
                    }
                }
                // "ping" => commands::ping::run(&command.data.options),
                // "id" => commands::id::run(&command.data.options),
                // "attachmentinput" => commands::attachmentinput::run(&command.data.options),
                _ => "not implemented :(".to_string(),
            };

            if let Err(why) = command
                .create_interaction_response(&ctx.http, |response| {
                    response
                        .kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|message| message.content(content).ephemeral(true))
                })
                .await
            {
                println!("Cannot respond to slash command: {}", why);
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);

        let guild_id = GuildId(
            env::var("GUILD_ID")
                .expect("Expected GUILD_ID in environment")
                .parse()
                .expect("GUILD_ID must be an integer"),
        );

        let commands = GuildId::set_application_commands(&guild_id, &ctx.http, |commands| {
            commands
                .create_application_command(|command| commands::configure::register(command))
        })
        .await;

        match commands {
            Ok(_) => println!("Guild commands created"),
            Err(error) => eprintln!("Error while creating commands: {}", error)
        }
    }
}

#[tokio::main]
async fn main() {
    // Load .env file
    dotenv().ok();

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");

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

    let bot = Bot {};

    // Build our client.
    let intents = 
        GatewayIntents::GUILD_MESSAGES |
        GatewayIntents::MESSAGE_CONTENT;
        
    let mut client = Client::builder(token, intents)
        .event_handler(bot)
        .await
        .expect("Error creating client");

    // Finally, start a single shard, and start listening to events.
    //
    // Shards will automatically attempt to reconnect, and will perform
    // exponential backoff until it reconnects.
    if let Err(why) = client.start().await {
        println!("Client error: {:?}", why);
    }
}