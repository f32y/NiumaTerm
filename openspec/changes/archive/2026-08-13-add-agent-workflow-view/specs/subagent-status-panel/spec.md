## MODIFIED Requirements

### Requirement: Share one right-side area
Git, `Background Tasks`, and `Workflows` SHALL select the same resizable right-side area. Selecting any of these views SHALL replace the currently shown one without opening a second right-side column, and all of them SHALL use the same current width and resize behavior.

#### Scenario: Switch from Git to Background Tasks
- **WHEN** Git is visible and the user selects `Background Tasks`
- **THEN** `Background Tasks` replaces Git within the existing right-side area and the main pane is not narrowed by another column

#### Scenario: Switch back to Git
- **WHEN** `Background Tasks` is visible and the user selects Git
- **THEN** Git replaces `Background Tasks` at the current right-side width

#### Scenario: Switch from Background Tasks to Workflows
- **WHEN** `Background Tasks` is visible and the user selects `Workflows`
- **THEN** `Workflows` replaces `Background Tasks` at the current right-side width and no second column opens
