#[cfg(test)]
mod tests {
    use crate::completion::dynamic::config_loader::DynamicConfigLoader;
    use crate::completion::json_loader::JsonCompletionLoader;

    #[test]
    fn test_load_new_json_completions() {
        let loader = JsonCompletionLoader::new();
        let completions = loader.list_available_completions().unwrap();

        assert!(
            completions.contains(&"grep".to_string()),
            "grep completion not found"
        );
        assert!(
            completions.contains(&"find".to_string()),
            "find completion not found"
        );
        assert!(
            completions.contains(&"mv".to_string()),
            "mv completion not found"
        );
        assert!(
            completions.contains(&"tar".to_string()),
            "tar completion not found"
        );
        assert!(
            completions.contains(&"ssh".to_string()),
            "ssh completion not found"
        );
    }

    #[test]
    fn test_load_grep_options() {
        let loader = JsonCompletionLoader::new();
        let completion = loader.load_command_completion("grep").unwrap().unwrap();

        let has_recursive = completion.global_options.iter().any(|opt| {
            opt.short.as_deref() == Some("-r") || opt.long.as_deref() == Some("--recursive")
        });
        assert!(has_recursive, "grep should have recursive option");
    }

    #[test]
    fn test_load_ssh_dynamic_config() {
        let configs = DynamicConfigLoader::load_all_configs().unwrap();
        let ssh_config = configs.iter().find(|c| c.command == "ssh");

        assert!(ssh_config.is_some(), "ssh dynamic config not found");
        let config = ssh_config.unwrap();
        assert!(
            config.shell_command.contains("awk"),
            "ssh command should use awk"
        );
    }
}
