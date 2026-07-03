#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct MimeTargets {
    pub(super) portal_filetransfer: u32,
    pub(super) portal_files: u32,
    pub(super) text_uri_list: u32,
    pub(super) text_uri_list_utf8: u32,
    pub(super) text_x_uri: u32,
    pub(super) kde_uri_list: u32,
    pub(super) gnome_copied_files: u32,
    pub(super) text_plain: u32,
    pub(super) text_plain_utf8: u32,
    pub(super) utf8_string: u32,
    pub(super) string: u32,
}

impl MimeTargets {
    pub(super) fn offered_targets(&self, portal_available: bool) -> Vec<u32> {
        let mut targets = Vec::with_capacity(11);

        if portal_available {
            self.push_unique(&mut targets, self.portal_filetransfer);
            self.push_unique(&mut targets, self.portal_files);
        }

        self.push_unique(&mut targets, self.text_uri_list);
        self.push_unique(&mut targets, self.text_uri_list_utf8);
        self.push_unique(&mut targets, self.text_x_uri);
        self.push_unique(&mut targets, self.kde_uri_list);
        self.push_unique(&mut targets, self.gnome_copied_files);

        self.push_unique(&mut targets, self.text_plain_utf8);
        self.push_unique(&mut targets, self.text_plain);
        self.push_unique(&mut targets, self.utf8_string);
        self.push_unique(&mut targets, self.string);

        targets
    }

    pub(super) fn enter_targets(&self, offered_targets: &[u32]) -> [u32; 3] {
        let mut selected = [0; 3];
        let mut len = 0;

        for &target in offered_targets {
            if self.is_file_payload_target(target) {
                Self::push_enter_target(&mut selected, &mut len, target);
                if len == selected.len() {
                    return selected;
                }
            }
        }

        for &target in offered_targets {
            if !self.is_file_payload_target(target) {
                Self::push_enter_target(&mut selected, &mut len, target);
                if len == selected.len() {
                    return selected;
                }
            }
        }

        selected
    }

    pub(super) fn is_portal_target(&self, target: u32) -> bool {
        target != 0 && (target == self.portal_filetransfer || target == self.portal_files)
    }

    pub(super) fn is_file_payload_target(&self, target: u32) -> bool {
        target != 0
            && (self.is_portal_target(target)
                || target == self.text_uri_list
                || target == self.text_uri_list_utf8
                || target == self.text_x_uri
                || target == self.kde_uri_list
                || target == self.gnome_copied_files)
    }

    fn push_unique(&self, targets: &mut Vec<u32>, target: u32) {
        if target != 0 && !targets.contains(&target) {
            targets.push(target);
        }
    }

    fn push_enter_target(selected: &mut [u32; 3], len: &mut usize, target: u32) {
        if target == 0 || selected.contains(&target) || *len == selected.len() {
            return;
        }

        selected[*len] = target;
        *len += 1;
    }
}
