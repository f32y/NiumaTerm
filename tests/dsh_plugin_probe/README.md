# dsh plugin probe

Tests whether a Cordis plugin NiumaTerm writes can expose the data an Agent Tab
needs from DeepSeek Harness, rather than being limited to what the published ACP
server chooses to forward. Background and results:
[`docs/research/deepseek-harness-integration.md`](../../docs/research/deepseek-harness-integration.md)
section 12.

Unlike the other probes here this one needs several npm packages, so it lives in
a directory with its own composition rather than as one file. It is hand-run,
not part of any suite: it exists to be repeated when `dsh` is upgraded, because
everything it checks belongs to that pre-release plugin runtime rather than to
this repository.

## What it proves

1. A plugin composed by **relative path** loads into a real composition.
2. `ctx.tools.get(name).presentResult(args, {content, isError, meta})`
   reproduces the render card the Web UI shows — the diff and terminal cards
   that the research initially assumed were reachable only over the Web `/api`
   transport.
3. An approval answerer registered by a plugin receives requests carrying a
   `callId` matching a tool call the plugin already observed, and its answer
   decides the outcome.

Claims 1 and 2 cost no model call: the plugin drives the tool registry itself.
Claim 3 needs one prompted turn, because the approval service refuses a question
raised outside an open turn.

## Running

```sh
cd tests/dsh_plugin_probe
npm init -y
npm install --no-audit --no-fund \
  @deepseek-ai/dsh-app-boot@next @deepseek-ai/dsh-llm-deepseek@next \
  @deepseek-ai/dsh-sandbox-local@next @deepseek-ai/dsh-sandbox-policy@next \
  @deepseek-ai/dsh-subprocess-local@next @deepseek-ai/dsh-fs-sandbox@next \
  @deepseek-ai/dsh-tool-fs@next @deepseek-ai/dsh-tool-str-replace-editor@next \
  @deepseek-ai/dsh-user-approval@next @deepseek-ai/dsh-agent-spine-demo@next

DEEPSEEK_API_KEY=... node run.mjs
cat nmt-probe.log        # the verdict is the last block
```

`node_modules`, `package.json`, the probe's log, and the files it edits are all
build output; keep them out of the commit.

## Composition notes

`dsh-fs-sandbox` replaces `dsh-fs-local` deliberately. The local backend applies
no approval policy at all, so nothing ever asks and claim 3 cannot be reached.
The sandbox policy runs in `workspace-write` so that an edit outside the
workspace root is denied, which is what makes the model retry with
`sandbox_permissions` and a justification — the one path on which the filesystem
tool asks for approval.
