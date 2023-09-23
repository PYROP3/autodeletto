mod commands;

use std::process::exit;
use std::env;

use dotenv::dotenv;

use log::{error, warn, info, debug};
use serenity::async_trait;
use serenity::model::application::interaction::{Interaction, InteractionResponseType};
use serenity::model::gateway::Ready;
use serenity::model::id::GuildId;
use serenity::model::prelude::Message;
use serenity::model::prelude::application_command::ApplicationCommandInteraction;
use serenity::prelude::*;

use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;

mod msgman;
use msgman::{MessageManagerReceiver,Command};

struct Bot {
    sender: Sender<Command>,
}

const QUEUE_LIMIT_MIN: i64 = 5;
const QUEUE_LIMIT_MAX: i64 = 500;

#[async_trait]
impl EventHandler for Bot {
    async fn message(&self, context: Context, message: Message) {
        if let Err(why) = self.sender.send(Command::MessageReceived { context, message }).await {
            error!("Error during sendcommand {}", why);
        }
    }

    async fn interaction_create(&self, context: Context, interaction: Interaction) {
        async fn reply(interaction:&ApplicationCommandInteraction, context: &Context, content: String, ephemeral: bool) {
            if let Err(why) = interaction
                .create_interaction_response(&context.http, |response| {
                    response
                        .kind(InteractionResponseType::ChannelMessageWithSource)
                        .interaction_response_data(|message| message.content(content).ephemeral(ephemeral))
                })
                .await
            {
                warn!("Cannot respond to slash command: {}", why);
            }
        }

        async fn defer(interaction:&ApplicationCommandInteraction, context: &Context, ephemeral: bool) {
            if let Err(why) = interaction
                .create_interaction_response(&context.http, |response| {
                    response
                        .kind(InteractionResponseType::DeferredChannelMessageWithSource)
                        .interaction_response_data(|message| message.ephemeral(ephemeral))
                })
                .await
            {
                warn!("Cannot defer slash command: {}", why);
            }
        }

        if let Interaction::ApplicationCommand(command) = interaction {
            info!("Received /{} from {} ({}) in {}", command.data.name, command.user.name, command.user.id, command.channel_id);
            match command.data.name.as_str() {
                "configure" => match commands::configure::run(&command.data.options) {
                    Err(_) => reply(&command, &context, "Please choose a valid number".to_string(), true).await,
                    Ok(limit) => {
                        if limit >= QUEUE_LIMIT_MIN && limit <= QUEUE_LIMIT_MAX {
                            defer(&command, &context, true).await;
                            if let Err(why) = self.sender.send(Command::SetLimit { limit: limit as usize, context, interaction: command }).await {
                                error!("Error during sendcommand {}", why);
                            }
                        } else {
                            reply(&command, &context, format!("The limit should be between {} and {}", QUEUE_LIMIT_MIN, QUEUE_LIMIT_MAX), true).await;
                        }
                    }
                }
                "remove" => {
                    defer(&command, &context, true).await;
                    if let Err(why) = self.sender.send(Command::RemoveLimit { context, interaction: command }).await {
                        error!("Error during sendcommand {}", why);
                    }
                }
                "status" => {
                    defer(&command, &context, true).await;
                    if let Err(why) = self.sender.send(Command::GetStatus { context, interaction: command }).await {
                        error!("Error during sendcommand {}", why);
                    }
                }
                "killswitch" => {
                    error!("User {} flipped the killswitch!", command.user.id);
                    reply(&command, &context, "Killswitch flipped, bye bye~".to_string(), true).await;
                    exit(1)
                }
                _ => reply(&command, &context, "not implemented :(".to_string(), true).await
            };
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!("{} is connected!", ready.user.name);

        // self.queue_manager.init(&ctx).await;

        let guild_id = GuildId(
            env::var("GUILD_ID")
                .expect("Expected GUILD_ID in environment")
                .parse()
                .expect("GUILD_ID must be an integer"),
        );

        let commands = GuildId::set_application_commands(&guild_id, &ctx.http, |commands| {
            commands
                .create_application_command(|command| commands::configure::register(command))
                .create_application_command(|command| commands::remove::register(command))
                .create_application_command(|command| commands::killswitch::register(command))
                .create_application_command(|command| commands::getstatus::register(command))
        })
        .await;

        match commands {
            Ok(_) => debug!("Guild commands created"),
            Err(error) => error!("Error while creating commands: {}", error)
        }

        debug!("Initializing message manager");
        if let Err(why) = self.sender.send(Command::Initialize { context: ctx }).await {
            error!("Error during sendcommand {}", why);
        }

        info!("Bot is ready!")
    }
}

#[tokio::main]
async fn main() {
    // Load .env file
    dotenv().ok();
    env_logger::init();
    info!("start main");

    // Configure the client with your Discord bot token in the environment.
    let token = env::var("DISCORD_TOKEN").expect("Expected a token in the environment");
    let (sender, receiver) = mpsc::channel::<Command>(32);

    let msgman = MessageManagerReceiver { };
    msgman.run(receiver);
    let bot = Bot {sender};

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
        error!("Client error: {:?}", why);
    }
}