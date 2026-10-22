pub(super) trait EventSource {
    fn supported_engines(&self) -> &'static [&'static str];
    fn policy_actions(&self) -> &'static [&'static str];
}
