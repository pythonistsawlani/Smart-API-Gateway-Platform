use serde::Serialize;

/// Platform shell type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RuntimeShell {
    /// Desktop shell (Windows/macOS/Linux GUI)
    Desktop,
    /// Mobile shell (Android/iOS)
    Mobile,
    /// Headless shell (headless / CLI mode)
    None,
}

/// Runtime startup plan, explicitly declaring whether each layer starts
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RuntimePlan {
    pub start_core: bool,
    pub start_server_api: bool,
    pub start_web_service: bool,
    pub shell: RuntimeShell,
}

impl RuntimePlan {
    /// Desktop shell plan: start all base runtime layers + desktop GUI
    pub fn desktop_shell() -> Self {
        Self {
            start_core: true,
            start_server_api: true,
            start_web_service: true,
            shell: RuntimeShell::Desktop,
        }
    }

    /// Mobile shell plan: start all base runtime layers + mobile GUI
    pub fn mobile_shell() -> Self {
        Self {
            start_core: true,
            start_server_api: true,
            start_web_service: true,
            shell: RuntimeShell::Mobile,
        }
    }

    /// Headless plan: start all base runtime layers, without GUI
    pub fn no_shell() -> Self {
        Self {
            start_core: true,
            start_server_api: true,
            start_web_service: true,
            shell: RuntimeShell::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RuntimePlan, RuntimeShell};

    #[test]
    fn mobile_shell_still_starts_base_runtime() {
        let plan = RuntimePlan::mobile_shell();
        assert!(plan.start_core);
        assert!(plan.start_server_api);
        assert!(plan.start_web_service);
        assert_eq!(plan.shell, RuntimeShell::Mobile);
    }

    #[test]
    fn no_shell_still_keeps_web_service_management_entry() {
        let plan = RuntimePlan::no_shell();
        assert!(plan.start_core);
        assert!(plan.start_server_api);
        assert!(plan.start_web_service);
        assert_eq!(plan.shell, RuntimeShell::None);
    }

    #[test]
    fn desktop_shell_starts_all_layers() {
        let plan = RuntimePlan::desktop_shell();
        assert!(plan.start_core);
        assert!(plan.start_server_api);
        assert!(plan.start_web_service);
        assert_eq!(plan.shell, RuntimeShell::Desktop);
    }

    #[test]
    fn all_plans_share_same_base_layers() {
        let desktop = RuntimePlan::desktop_shell();
        let mobile = RuntimePlan::mobile_shell();
        let no_shell = RuntimePlan::no_shell();

        assert_eq!(desktop.start_core, mobile.start_core);
        assert_eq!(desktop.start_core, no_shell.start_core);
        assert_eq!(desktop.start_server_api, mobile.start_server_api);
        assert_eq!(desktop.start_server_api, no_shell.start_server_api);
        assert_eq!(desktop.start_web_service, mobile.start_web_service);
        assert_eq!(desktop.start_web_service, no_shell.start_web_service);
    }
}
