use super::*;
use crate::test_support::TestShellProxy;

fn change_directory(proxy: &mut impl ShellNavigation, path: &str) -> Result<()> {
    proxy.changepwd(path)
}

struct NavigationFake {
    changed_to: Option<String>,
}

impl ShellNavigation for NavigationFake {
    fn save_path_history(&mut self, _path: &str) {}
    fn changepwd(&mut self, path: &str) -> Result<()> {
        self.changed_to = Some(path.to_string());
        Ok(())
    }
    fn dir_stack(&self) -> Vec<String> {
        Vec::new()
    }
    fn dir_stack_set(&mut self, _stack: Vec<String>) {}
    fn get_current_dir(&self) -> Result<PathBuf> {
        Ok(PathBuf::from("/fake"))
    }
    fn add_dir_alias(&mut self, _name: String, _path: String) -> bool {
        false
    }
    fn remove_dir_alias(&mut self, _name: &str) -> bool {
        false
    }
    fn list_dir_aliases(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    fn get_dir_alias(&self, _name: &str) -> Option<String> {
        None
    }
}

#[test]
fn legacy_shell_proxy_receives_capability_adapters() {
    let mut proxy = TestShellProxy {
        current_dir: PathBuf::from("/work"),
        allow_changepwd: true,
        ..TestShellProxy::default()
    };

    change_directory(&mut proxy, "/next").unwrap();
    assert_eq!(proxy.changed_to.as_deref(), Some("/next"));
}

#[test]
fn helper_can_use_capability_only_fake() {
    let mut fake = NavigationFake { changed_to: None };

    change_directory(&mut fake, "/next").unwrap();
    assert_eq!(fake.changed_to.as_deref(), Some("/next"));
}

#[test]
fn execution_capability_keeps_fail_closed_defaults() {
    let mut proxy = TestShellProxy {
        current_dir: PathBuf::from("/work"),
        ..TestShellProxy::default()
    };

    assert!(!ShellExecution::confirm_action(&mut proxy, "dangerous").unwrap());
}
