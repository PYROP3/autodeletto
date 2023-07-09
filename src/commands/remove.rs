use serenity::builder;

pub fn register(
    command: &mut builder::CreateApplicationCommand,
) -> &mut builder::CreateApplicationCommand {
    command
        .name("remove")
        .description("Remove autodelete for this channel")
}