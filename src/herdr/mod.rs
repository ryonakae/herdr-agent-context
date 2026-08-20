pub mod protocol;
pub mod socket;

use protocol::{AgentInfo, ProcessInfo, SessionSnapshot, TabInfo};

pub struct MetadataReport<'a> {
    pub agent: &'a str,
    pub pane_id: &'a str,
    pub applies_to_source: Option<&'a str>,
    pub seq: u64,
    pub ttl_ms: u64,
    pub session_name: Option<&'a str>,
    pub last_message: Option<&'a str>,
}

pub trait HerdrApi {
    type Error;

    fn is_missing_pane_error(_error: &Self::Error) -> bool {
        false
    }

    fn is_missing_tab_error(_error: &Self::Error) -> bool {
        false
    }

    fn list_agents(&mut self) -> Result<Vec<AgentInfo>, Self::Error>;
    fn process_info(&mut self, pane_id: &str) -> Result<ProcessInfo, Self::Error>;
    fn session_snapshot(&mut self) -> Result<SessionSnapshot, Self::Error>;
    fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<TabInfo, Self::Error>;
    fn report_metadata(&mut self, report: MetadataReport<'_>) -> Result<(), Self::Error>;
}

impl HerdrApi for socket::SocketTransport {
    type Error = socket::SocketError;

    fn is_missing_pane_error(error: &Self::Error) -> bool {
        matches!(
            error,
            socket::SocketError::Api(code) if code == "pane_not_found" || code == "unknown_pane"
        )
    }

    fn is_missing_tab_error(error: &Self::Error) -> bool {
        matches!(error, socket::SocketError::Api(code) if code == "tab_not_found")
    }

    fn list_agents(&mut self) -> Result<Vec<AgentInfo>, Self::Error> {
        self.list_agents()
    }

    fn process_info(&mut self, pane_id: &str) -> Result<ProcessInfo, Self::Error> {
        self.process_info(pane_id)
    }

    fn session_snapshot(&mut self) -> Result<SessionSnapshot, Self::Error> {
        self.session_snapshot()
    }

    fn rename_tab(&mut self, tab_id: &str, label: &str) -> Result<TabInfo, Self::Error> {
        self.rename_tab(tab_id, label)
    }

    fn report_metadata(&mut self, report: MetadataReport<'_>) -> Result<(), Self::Error> {
        self.report_metadata(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_only_tab_not_found_as_a_missing_tab() {
        assert!(<socket::SocketTransport as HerdrApi>::is_missing_tab_error(
            &socket::SocketError::Api("tab_not_found".into())
        ));
        assert!(
            !<socket::SocketTransport as HerdrApi>::is_missing_tab_error(
                &socket::SocketError::Api("workspace_not_found".into())
            )
        );
    }
}
