#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct RecentRealTarget;

impl RecentRealTarget {
    pub(super) fn note_entered_real(&mut self, _target: u32) {}
    pub(super) fn note_data_request(&mut self, _target: u32) {}
    pub(super) fn note_status_accept(&mut self, _target: u32) {}
    pub(super) fn note_status_reject(&mut self, _target: u32) {}
}
