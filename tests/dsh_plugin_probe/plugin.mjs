// A NiumaTerm-owned Cordis plugin for DeepSeek Harness, written to test whether
// a plugin we control can expose the data an Agent Tab needs. See
// docs/research/deepseek-harness-integration.md section 12.
//
// It proves three things, and prints a verdict for each:
//
//   1. a plugin composed by RELATIVE PATH loads into a real composition;
//   2. `ctx.tools.get(name).presentResult(args, {content, isError, meta})`
//      reproduces the render card the Web UI shows, from the arguments and
//      `meta` a `tool/call` / `tool/result` pair carries;
//   3. an approval answerer registered by a plugin receives requests carrying a
//      callId matching a tool call the plugin already observed, and its answer
//      decides the outcome.
//
// Claims 1 and 2 are reached without a model call, by driving the tool registry
// directly. Claim 3 needs one prompted turn, because the approval service
// refuses a question raised outside an open turn.

import { appendFileSync, writeFileSync } from 'node:fs'
import { createUserMessage } from '@deepseek-ai/dsh-llm'

const LOG = process.env.NMT_PROBE_LOG ?? './nmt-probe.log'
const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

function log(...parts) {
  appendFileSync(
    LOG,
    parts.map((p) => (typeof p === 'string' ? p : JSON.stringify(p, null, 2))).join(' ') + '\n',
  )
}

export const name = 'nmt-probe'
// This Cordis build takes an array. The `{required, optional}` object form is
// read as two services literally named `required` and `optional`, and the
// plugin then waits for them forever.
export const inject = ['tools', 'approval', 'agents']

export function apply(ctx) {
  writeFileSync(LOG, '')
  log('=== CLAIM 1: a plugin composed by relative path loads')
  log('apply() ran')
  // Reading a service key the plugin did not inject throws rather than
  // returning undefined, so capability probing needs the guard.
  const has = (k) => {
    try {
      return ctx[k] ? 'yes' : 'no'
    } catch {
      return 'not injected'
    }
  }
  log(
    'services:',
    ['tools', 'agents', 'approval', 'userQuestions', 'sessionQuery', 'sessionProjections', 'subagents']
      .map((k) => `${k}=${has(k)}`)
      .join('  '),
  )

  // The composition is not settled inside apply().
  setTimeout(() => {
    probe(ctx).catch((e) => log('PROBE FAILED:', String((e && e.stack) || e)))
  }, 1500)
}

async function probe(ctx) {
  const verdict = { one: 'YES', two: null, three: null }

  // Presenters are declared per tool, not per composition.
  log('\n=== registry: which tools declare presenters')
  const schemas = ctx.tools.schemas()
  for (const s of schemas) {
    const def = ctx.tools.get(s.name)
    log(
      `  ${s.name.padEnd(20)} presentCall=${typeof def?.presentCall === 'function'}` +
        ` presentResult=${typeof def?.presentResult === 'function'}` +
        ` presentationMeta=${typeof def?.output?.presentationMeta === 'function'}`,
    )
  }

  // Calls and results are kept by callId: a turn makes several, and pairing the
  // newest call with the newest result compares unrelated ones.
  const calls = new Map()
  const results = new Map()
  const kinds = new Map()
  // The listener is called as (session, event), with the event shaped
  // {type, seq, time, data}.
  ctx.on('session/event', (_session, event) => {
    const { type, data } = event ?? {}
    if (!type) return
    kinds.set(type, (kinds.get(type) ?? 0) + 1)
    if (type === 'tool/call') calls.set(String(data.callId), data)
    if (type === 'tool/result') {
      const id = data.message?.source?.callId
      if (id) results.set(String(id), data)
    }
    if (type === 'turn/end') log('\n[turn/end]', JSON.stringify(data).slice(0, 400))
  })

  // CLAIM 3, first half: register the answerer.
  const approvals = []
  ctx.on('approval/request', async (req) => {
    approvals.push({
      toolName: req.toolName,
      callId: req.callId ?? null,
      reason: req.reason ?? null,
      agent: req.agent ? 'present' : null,
    })
    log('\n=== CLAIM 3: the answerer was called with', approvals.at(-1))
    // The vocabulary is allowed-once | rejected | cancelled | unavailable.
    // ACP's `allow-once` spelling is not a member and does not fail loudly: the
    // waterfall falls through to the fail-closed default and the request is
    // refused, which looks like the answerer never ran.
    return 'allowed-once'
  })

  // An agent is needed for the approval question to be asked on behalf of, and
  // for tool executions to reach a session log. Creating one costs no model call.
  const handle = await ctx.agents.create({
    sessionId: 'nmt-probe-session-1',
    // The DeepSeek provider registers as `deepseek-official`. An unknown
    // provider does not fail at boot: the turn opens and ends with NO_ADAPTER.
    agentOptions: { provider: 'deepseek-official', model: 'deepseek-v4-flash' },
    meta: { cwd: process.cwd() },
  })
  const agent = handle.agent ?? handle
  log('\nagent created without prompting; hasSession =', Boolean(agent?.session))

  // CLAIM 2, without a model: drive the registry directly.
  const inside = `${process.cwd().replace(/\\/g, '/')}/nmt-probe-target.txt`
  writeFileSync(inside, 'line one\nbefore\nline three\n')
  const args = { file_path: inside, old_string: 'before', new_string: 'after' }
  log('\n=== CLAIM 2 (registry driven directly): edit', args)
  const settled = await ctx.tools.execute({
    callId: 'nmt-probe-call-1',
    name: 'edit',
    arguments: args,
    agent,
    signal: AbortSignal.timeout(30000),
  })
  const edit = ctx.tools.get('edit')
  log('presentCall   ->', edit.presentCall?.(args) ?? null)
  const direct = edit.presentResult?.(args, {
    content: settled.content ?? [],
    isError: Boolean(settled.isError),
    ...(settled.meta === undefined ? {} : { meta: settled.meta }),
  })
  log('presentResult ->', direct ?? null)
  verdict.two = direct ? `YES (card=${direct.card})` : 'NO'

  // CLAIM 3, second half, plus CLAIM 2 against real events. The task is chosen
  // so the sandbox fence denies it first: the filesystem tool asks for approval
  // only on the one permitted wider retry.
  const denied = `${process.cwd().replace(/\\/g, '/')}/../nmt-probe-denied.txt`
  writeFileSync(denied, 'guarded one\nbefore\nguarded three\n')
  log(`\n=== CLAIM 3: prompting one real turn to edit ${denied}`)
  agent.followup(
    createUserMessage({
      content: [
        {
          type: 'text',
          text:
            `Use the edit tool to change the word "before" to "after" in the file ${denied}. ` +
            `That path is outside your writable workspace, so the first attempt will be denied ` +
            `with FS_SANDBOX_DENIED. When it is, immediately retry the SAME edit once, passing ` +
            `sandbox_permissions: "danger-full-access" and a one-sentence justification. ` +
            `Do not give up and do not ask me anything.`,
        },
      ],
      source: { kind: 'user' },
    }),
  )
  for (let i = 0; i < 90 && approvals.length === 0; i++) await sleep(1000)
  await sleep(20000)

  log('\n=== CLAIM 2 (from real tool/call + tool/result events)')
  for (const [id, call] of calls) {
    const res = results.get(id)
    const def = ctx.tools.get(call.name)
    if (!res || !def?.presentResult) continue
    // The event's message wraps the model-facing blocks inside one tool-result
    // block; the presenter is declared against the inner blocks, so passing the
    // wrapper returns null and reads as a missing card.
    const wrapper = res.message?.content?.find((b) => b?.type === 'tool-result')
    const view = def.presentResult(JSON.parse(call.arguments), {
      content: wrapper?.content ?? [],
      isError: Boolean(res.error ?? wrapper?.isError),
      ...(res.meta === undefined ? {} : { meta: res.meta }),
    })
    // A failed call carries no meta and presents nothing, so the generic
    // fallback is the normal path for every denied or errored call.
    log(
      `\n-- ${call.name} ${id} meta=${res.meta ? Object.keys(res.meta).join(',') : 'none'}` +
        ` isError=${Boolean(res.error)}`,
    )
    log('   presentResult ->', view ?? null)
  }

  const matched = approvals[0]?.callId ? calls.has(String(approvals[0].callId)) : false
  verdict.three = approvals.length
    ? `YES (callId ${matched ? 'matches' : 'does NOT match'} an observed tool/call)`
    : 'NOT TRIGGERED'

  log('\n=== event types this turn produced')
  log([...kinds.entries()].map(([k, n]) => `${k}=${n}`).join('  '))

  log('\n=== VERDICT')
  log('  claim 1  plugin loads by relative path :', verdict.one)
  log('  claim 2  presenter reproduces the card :', verdict.two)
  log('  claim 3  answerer sees the tool call   :', verdict.three)
}
