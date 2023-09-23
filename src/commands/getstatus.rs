use serenity::builder;

pub fn register(
    command: &mut builder::CreateApplicationCommand,
) -> &mut builder::CreateApplicationCommand {
    command
        .name("status")
        .description("Collect data about managed channels")
}