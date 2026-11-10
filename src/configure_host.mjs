// Runs a package's configure() hook. Prefixed by codemods.rs with
// `const INPUT = JSON.parse(...)`: { name, sub, root, force, flags, parsedFlags }.
//
// The hook receives a command object, as upstream's `configure` command passes
// itself: prompt, parsedFlags, parsedArgs, logger, ui, colors, createCodemods(),
// exitCode, force, name, stubsRoot. That object also inherits the codemods, and
// the raw flags stay the second argument, so a hook written
// `configure(codemods, flags)` runs unchanged.
import { createCodemods, Prompt, Ui } from '@c9up/ream'

let hook
let source
try {
  source = await import(INPUT.sub)
  hook = source.configure
} catch {}
if (typeof hook !== 'function') {
  source = await import(INPUT.root)
  hook = source.configure
}

function runningInCI() {
  const value = process.env.CI
  return value !== undefined && value !== '' && value !== '0' && value.toLowerCase() !== 'false'
}

// No terminal to read from, or a CI job: a prompt is answered by its default,
// or fails at once. Waiting would hang the job until its timeout.
const interactive = Boolean(process.stdin.isTTY) && !runningInCI()

function notInteractive(message) {
  const error = new Error(
    `Cannot ask "${message}": this run is not interactive (${runningInCI() ? 'CI' : 'stdin is not a terminal'}).`,
  )
  error.code = 'E_CONSOLE_NOT_INTERACTIVE'
  error.hint = `Pass the answer as a flag to \`ream configure ${INPUT.name}\`, or run it in a terminal.`
  return error
}

async function accepted(message, value, options) {
  if (typeof options.validate === 'function') {
    const verdict = await options.validate(value)
    if (verdict !== true) {
      const error = new Error(
        `The default answer to "${message}" is rejected: ${typeof verdict === 'string' ? verdict : 'invalid value'}.`,
      )
      error.code = 'E_CONSOLE_NOT_INTERACTIVE'
      throw error
    }
  }
  return typeof options.result === 'function' ? options.result(value) : value
}

function nameOf(choice) {
  return typeof choice === 'string' ? choice : choice.name
}

function picked(message, choices, index) {
  if (!Number.isInteger(index) || index < 0 || index >= choices.length) {
    const error = new Error(`The default of "${message}" points at no option (index ${index}).`)
    error.code = 'E_CONSOLE_NOT_INTERACTIVE'
    throw error
  }
  return nameOf(choices[index])
}

class ConfigurePrompt extends Prompt {
  async ask(message, options = {}) {
    if (interactive) return super.ask(message, options)
    if (options.default === undefined) throw notInteractive(message)
    return accepted(message, options.default, options)
  }

  async secure(message, options = {}) {
    if (interactive) return super.secure(message, options)
    if (options.default === undefined) throw notInteractive(message)
    return accepted(message, options.default, options)
  }

  async confirm(message, options = {}) {
    if (interactive) return super.confirm(message, options)
    if (options.default === undefined) throw notInteractive(message)
    return accepted(message, Boolean(options.default), options)
  }

  async toggle(message, labels, options = {}) {
    if (interactive) return super.toggle(message, labels, options)
    if (options.default === undefined) throw notInteractive(message)
    return accepted(message, Boolean(options.default), options)
  }

  async choice(message, choices, options = {}) {
    if (interactive) return super.choice(message, choices, options)
    if (options.default === undefined) throw notInteractive(message)
    const index = Array.isArray(options.default) ? options.default[0] : options.default
    return accepted(message, picked(message, choices, index), options)
  }

  async autocomplete(message, choices, options = {}) {
    if (interactive) return super.autocomplete(message, choices, options)
    if (options.default === undefined) throw notInteractive(message)
    const index = Array.isArray(options.default) ? options.default[0] : options.default
    return accepted(message, picked(message, choices, index), options)
  }

  async multiple(message, choices, options = {}) {
    if (interactive) return super.multiple(message, choices, options)
    if (options.default === undefined) throw notInteractive(message)
    const indexes = Array.isArray(options.default) ? options.default : [options.default]
    return accepted(
      message,
      indexes.map((index) => picked(message, choices, index)),
      options,
    )
  }
}

const codemods = createCodemods({ force: INPUT.force })
const ui = new Ui()
const command = Object.assign(Object.create(codemods), {
  name: INPUT.name,
  force: INPUT.force,
  parsedFlags: INPUT.parsedFlags,
  parsedArgs: [],
  prompt: new ConfigurePrompt(),
  ui,
  logger: ui.logger,
  colors: ui.colors,
  exitCode: undefined,
  stubsRoot: source.stubsRoot,
  async createCodemods() {
    return codemods
  },
})

try {
  await hook(command, INPUT.flags)
} catch (error) {
  // A framework error says what to do; its stack says nothing more.
  if (error && typeof error.code === 'string' && error.code.startsWith('E_')) {
    process.stderr.write(`${error.code}: ${error.message}\n`)
    if (typeof error.hint === 'string') process.stderr.write(`  ${error.hint}\n`)
    process.exit(1)
  }
  throw error
}
if (typeof command.exitCode === 'number' && command.exitCode !== 0) {
  process.exitCode = command.exitCode
}
