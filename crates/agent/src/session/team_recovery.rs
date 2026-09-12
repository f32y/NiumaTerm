/// A completed root reply from the resumed provider conversation. Only an
/// exact provider turn identifier can associate it with a saved Team request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveredTeamTurn {
    pub id: String,
    pub text: String,
}
