use super::*;
use clap::CommandFactory;

#[test]
fn scoped_global_option_registry_matches_clap_arguments() {
    let command = Cli::command();
    for option in COMMAND_SCOPE_OPTIONS {
        assert!(
            command
                .get_arguments()
                .any(|argument| argument.get_id().as_str() == option.id),
            "scoped option {} is not defined by Clap",
            option.id
        );
    }
}
