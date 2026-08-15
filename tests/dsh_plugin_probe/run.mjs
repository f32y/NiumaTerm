// Boot the composition beside this file. `dsh-app-boot` is the same entry the
// published ACP and SDK server bins use, so the plugin runs in a real tree
// rather than a harness invented for the test.
import { boot } from '@deepseek-ai/dsh-app-boot'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
await boot('nmt-probe', resolve(here, 'cordis.yml'))
// The tree stays alive on its own; the probe is done well inside this.
setTimeout(() => process.exit(0), 180000)
