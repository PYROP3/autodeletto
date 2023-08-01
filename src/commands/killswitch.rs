use serenity::builder;

pub fn register(
    command: &mut builder::CreateApplicationCommand,
) -> &mut builder::CreateApplicationCommand {
    command
        .name("killswitch")
        .description("Kill this bot if it starts behaving unexpectedly")
}