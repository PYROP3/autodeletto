use serenity::builder;
use serenity::model::prelude::command::CommandOptionType;
use serenity::model::prelude::interaction::application_command::{
    CommandDataOption,
    CommandDataOptionValue,
};

pub fn register(
    command: &mut builder::CreateApplicationCommand,
) -> &mut builder::CreateApplicationCommand {
    command
        .name("configure")
        .description("Configure autodelete for this channel")
        .create_option(|option| {
            option
                .name("messages")
                .description("How many messages to keep")
                .kind(CommandOptionType::Integer)
                .required(true)
        })
}

pub fn run(options: &[CommandDataOption]) -> Result<i64, ()> {
    let option = options
        .get(0)
        .expect("Expected messages option")
        .resolved
        .as_ref()
        .expect("Expected user object");    
    if let CommandDataOptionValue::Integer(i) = option {
        Ok(*i)
    } else {
        Err(())
    }
    // "Hey, I'm alive!".to_string()
}