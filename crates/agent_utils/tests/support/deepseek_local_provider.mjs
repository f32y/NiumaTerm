// Runs the real installed dsh against a local model; no provider account is used.
// From the repository root: node crates/agent_utils/tests/support/deepseek_local_provider.mjs
// An optional argument selects one test. The default runs the protocol scenarios.
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const server = createServer(async (request, response) => {
  let body = '';
  for await (const chunk of request) body += chunk;
  const input = JSON.parse(body || '{}');
  if (request.url.endsWith('/models')) {
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ object: 'list', data: [{ id: 'deepseek-chat', object: 'model' }] }));
    return;
  }
  const messages = input.messages || [];
  const prompt = messages.filter(message => message.role === 'user').map(message => typeof message.content === 'string' ? message.content : message.content?.map(part => part.text || '').join('')).join('\n');
  const completed = messages.filter(message => message.role === 'tool');
  console.log('MODEL', request.url, 'tool results', completed.length);
  response.setHeader('content-type', 'text/event-stream');
  const emit = (delta, finish_reason = null) => response.write(`data: ${JSON.stringify({ id: 'probe', object: 'chat.completion.chunk', model: input.model, choices: [{ index: 0, delta, finish_reason }] })}\n\n`);
  emit({ role: 'assistant', content: '' });
  if (input.tools && prompt.includes('protocol-probe question') && completed.length === 0) {
    const args = { questions: [{ id: 'probe-choice', question: 'Continue this test?', options: [{ label: 'Yes' }, { label: 'No' }] }] };
    emit({ tool_calls: [{ index: 0, id: 'probe-question', type: 'function', function: { name: 'ask_user_question', arguments: JSON.stringify(args) } }] });
    emit({}, 'tool_calls');
    response.end('data: [DONE]\n\n');
  } else if (input.tools && prompt.includes('approval-probe-ok') && completed.length < 2) {
    const path = prompt.match(/approval-probe-ok to (.+?) using/)?.[1];
    const args = { command: `Set-Content -LiteralPath '${path}' -Value 'approval-probe-ok'`, description: 'Write the isolated approval marker' };
    if (completed.length) Object.assign(args, { sandbox_permissions: 'danger-full-access', justification: 'Allow writing the isolated approval marker outside the test workspace.' });
    emit({ tool_calls: [{ index: 0, id: `approval-${completed.length}`, type: 'function', function: { name: 'pwsh', arguments: JSON.stringify(args) } }] });
    emit({}, 'tool_calls');
    response.end('data: [DONE]\n\n');
  } else if (input.tools && prompt.includes('Do exactly two things') && completed.length < 3) {
    const path = prompt.match(/Second, edit (.+?) replacing/)?.[1];
    const calls = [
      ['pwsh', { command: "Write-Output 'tool-probe-ok'", description: 'Print a test marker' }],
      ['read', { file_path: path }],
      ['edit', { file_path: path, old_string: 'before', new_string: 'after' }],
    ];
    const [name, args] = calls[completed.length];
    emit({ tool_calls: [{ index: 0, id: `probe-${completed.length}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }] });
    emit({}, 'tool_calls');
    response.end('data: [DONE]\n\n');
  } else if (input.tools && prompt.includes('Count from') && !prompt.includes('Abandon the counting')) {
    let count = 0;
    const timer = setInterval(() => emit({ content: `${++count}: local streamed output\n` }), 100);
    response.on('close', () => clearInterval(timer));
  } else {
    emit({ content: 'ok' });
    emit({}, 'stop');
    response.end('data: [DONE]\n\n');
  }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const scenarios = process.argv[2] ? [process.argv[2]] : [
  'a_session_opens_and_receives_its_preset_catalog',
  'a_turn_streams_and_survives_being_stopped',
  'a_real_turn_shows_its_commands_and_file_changes',
  'an_approval_is_raised_answered_and_the_turn_continues',
  'a_question_is_answered_and_the_turn_continues',
  'two_sessions_share_one_host_and_do_not_see_each_other',
  'a_profile_can_declare_and_select_an_image_model',
];
try {
  for (const scenario of scenarios) {
    const probeHome = mkdtempSync(join(tmpdir(), 'nmt-dsh-protocol-'));
    try {
      const code = await new Promise((resolve, reject) => {
        const child = spawn('cargo', ['test', '-p', 'nmt_agent_utils', '--test', 'deepseek_live', scenario, '--', '--ignored', '--nocapture'], {
          windowsHide: true, stdio: 'inherit',
          env: { ...process.env, DSH_HOME: probeHome, DEEPSEEK_API_KEY: 'local-probe', DEEPSEEK_BASE_URL: `http://127.0.0.1:${server.address().port}` },
        });
        child.once('error', reject);
        child.once('exit', resolve);
      });
      if (code !== 0) { process.exitCode = code ?? 1; break; }
    } finally {
      rmSync(probeHome, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
    }
  }
} finally {
  server.closeAllConnections();
  server.close();
}
