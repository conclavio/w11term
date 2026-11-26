use super::*;

const SORT_ORDERS: [(SortOrder, &str); 3] = [
    (SortOrder::Name, "Sort by name"),
    (SortOrder::NewestFirst, "Newest first"),
    (SortOrder::LargestFirst, "Largest first"),
];

impl FileDrawer {
    pub(super) fn sorting_control(
        &self,
        pane: u64,
        context: &ViewContext<TerminalApp>,
    ) -> ComboBox {
        let generation = self.generation;
        let revision = self.listing_revision;
        let current = self.sort_order;
        ComboBox::new()
            .automation_name("Sort files")
            .min_width(0.0)
            .is_enabled(!self.busy() && self.directory.is_some())
            .items_source(SORT_ORDERS.map(|(_, label)| label))
            .selected_index(SORT_ORDERS.iter().position(|&(order, _)| order == current))
            .on_selection_changed(context.callback(move |index: Option<usize>| {
                index
                    .and_then(|index| SORT_ORDERS.get(index))
                    .filter(|&&(order, _)| order != current)
                    .map_or(Message::ControlIdle, |&(order, _)| {
                        Message::Files(FileAction::Sort(pane, generation, revision, order))
                    })
            }))
    }

    pub(super) fn listing_options(&self, pane: u64, context: &ViewContext<TerminalApp>) -> View {
        use FileAction::*;
        let generation = self.generation;
        let ready = !self.busy();
        let follow = checkbox(
            "Follow shell directory",
            true,
            self.following,
            move |checked| Follow(pane, generation, checked),
            context,
        );
        let replace = checkbox(
            "Replace existing files",
            ready,
            self.replace,
            move |checked| Replace(pane, generation, checked),
            context,
        );
        let download_times = checkbox(
            "Preserve modification times",
            ready,
            self.preserve_mtime,
            move |checked| PreserveMtime(pane, generation, checked),
            context,
        );
        let atomic = checkbox(
            "Upload files atomically",
            ready,
            self.atomic_uploads,
            move |checked| AtomicUploads(pane, generation, checked),
            context,
        );
        let upload_times = checkbox(
            "Preserve upload timestamps",
            ready,
            self.preserve_upload_times,
            move |checked| PreserveUploadTimes(pane, generation, checked),
            context,
        );
        let summary = match (self.following, self.replace) {
            (true, true) => "Following shell · Overwrite on",
            (true, false) => "Following shell",
            (false, true) => "Overwrite on",
            (false, false) => "",
        };
        let header = StackPanel::new().spacing(2.0).children((
            TextBlock::new().text("File options"),
            if summary.is_empty() {
                View::empty()
            } else {
                TextBlock::new().text(summary).font_size(12.0).into()
            },
        ));
        Expander::new()
            .automation_name("File options")
            .is_expanded(self.options_expanded)
            .on_is_expanded_changed(context.callback(move |expanded| {
                Message::Files(OptionsExpanded(pane, generation, expanded))
            }))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .slots([
                SlotView::new(ExpanderSlot::Header, header),
                SlotView::new(
                    ExpanderSlot::Content,
                    StackPanel::new().spacing(4.0).children((
                        follow,
                        TextBlock::new()
                            .text("Transfers")
                            .margin(Thickness::new(0.0, 6.0, 0.0, 0.0)),
                        replace,
                        download_times,
                        atomic,
                        upload_times,
                    )),
                ),
            ])
    }
}

fn checkbox(
    label: &str,
    enabled: bool,
    checked: bool,
    action: impl Fn(bool) -> FileAction + 'static,
    context: &ViewContext<TerminalApp>,
) -> View {
    CheckBox::new()
        .automation_name(label)
        .is_enabled(enabled)
        .is_checked(checked)
        .on_is_checked_changed(context.callback(move |checked| Message::Files(action(checked))))
        .content(label)
}
