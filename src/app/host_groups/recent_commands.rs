use super::*;

const LIMIT: usize = 50;

#[derive(Default)]
pub(super) struct RecentCommands {
    commands: VecDeque<(u64, Arc<str>)>,
}

impl RecentCommands {
    pub(super) fn record(&mut self, submission: u64, command: Arc<str>) {
        self.commands.retain(|(_, previous)| previous != &command);
        self.commands.push_front((submission, command));
        self.commands.truncate(LIMIT);
    }

    pub(super) fn get(&self, submission: u64) -> Option<&str> {
        self.commands
            .iter()
            .find(|(id, _)| *id == submission)
            .map(|(_, text)| text.as_ref())
    }

    fn matches(&self, query: &str) -> impl Iterator<Item = (u64, &str)> {
        let needle = query.trim().to_lowercase();
        self.commands
            .iter()
            .filter(move |(_, text)| needle.is_empty() || text.to_lowercase().contains(&needle))
            .map(|(id, text)| (*id, text.as_ref()))
    }
}

impl TerminalApp {
    pub(in crate::app) fn recent_group_commands(
        &self,
        group: TabId,
        query: &str,
    ) -> impl Iterator<Item = (u64, &str)> {
        let page = match self.tabs.get(group).map(|tab| &tab.page) {
            Some(Page::HostGroup(page)) if self.host_group_command_target() == Some(group) => {
                Some(page)
            }
            _ => None,
        };
        page.into_iter()
            .flat_map(move |page| page.recent_commands.matches(query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_commands_are_bounded_and_match_the_entire_exact_line() {
        let mut group = HostGroup::default();
        let submit = |group: &mut HostGroup, content| {
            group.submit(content, Vec::new(), GroupTranscript::default());
        };
        let command = format!("{}  [ÉTAT]", "output".repeat(90));
        submit(
            &mut group,
            SubmissionContent::Command(command.as_str().into()),
        );
        for id in 2..=LIMIT {
            submit(
                &mut group,
                SubmissionContent::Command(format!("run {id}").into()),
            );
        }
        submit(&mut group, SubmissionContent::Environment(BTreeMap::new()));
        assert_eq!(group.recent_commands.matches("").count(), LIMIT);
        assert_eq!(
            group.recent_commands.matches(" [état] ").next(),
            Some((1, command.as_str()))
        );
        assert_eq!(group.recent_commands.matches("group variables").count(), 0);
        assert!(
            group
                .submissions
                .iter()
                .all(|submission| submission.id != 1)
        );
        submit(&mut group, SubmissionContent::Command("run latest".into()));
        assert!(group.recent_commands.get(1).is_none());
        assert_eq!(group.recent_commands.get(2), Some("run 2"));

        submit(&mut group, SubmissionContent::Command("run 2".into()));
        assert!(group.recent_commands.get(2).is_none());
        assert_eq!(
            group.recent_commands.matches("run 2").next(),
            Some((LIMIT as u64 + 3, "run 2"))
        );
        assert_eq!(group.recent_commands.matches("").count(), LIMIT);
    }
}
