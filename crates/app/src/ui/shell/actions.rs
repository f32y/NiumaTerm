use gpui::actions;

actions!(
    NiumaTerm,
    [
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        NewWorkspace,
        NextWorkspace,
        PrevWorkspace,
        NewWindow,
        SplitUp,
        SplitDown,
        SplitLeft,
        SplitRight,
        ResizePaneUp,
        ResizePaneDown,
        ResizePaneLeft,
        ResizePaneRight,
        ToggleSidebar,
        ToggleGitSidebar,
        ShowSettings,
        NewRemoteTab,
        NewAgentTab,
    ]
);
