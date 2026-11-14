import { expect, test } from 'claude-code/testing'

import { cockpitCommand, home } from '../hooks/config'
import { changesArgs, find, searchArgs } from '../hooks/find'

const ran = (exitCode: number, stdout = '', stderr = '') => ({ exitCode, stdout, stderr, isStdoutTruncated: false, isStderrTruncated: false })

test('a search needs words, trimmed', async () => {
  expect(searchArgs({ query: '  ENG-401 ' })).toEqual(['search', 'ENG-401'])
  expect(searchArgs({ query: '   ' })).toContain('query:')
  expect(searchArgs({})).toContain('query:')
})

test('the changes take a duration or epoch milliseconds, and inbox or monitor', async () => {
  expect(changesArgs({})).toEqual(['events'])
  expect(changesArgs({ since: '30m', app: 'monitor' })).toEqual(['events', '--since', '30m', '--app', 'monitor'])
  expect(changesArgs({ since: 1761660000000 })).toEqual(['events', '--since', '1761660000000'])
  expect(changesArgs({ since: 'this morning' })).toContain('since:')
  expect(changesArgs({ since: '12' })).toContain('since:')
  expect(changesArgs({ app: 'todo' })).toContain('app:')
})

test('the CLI runs through cockpit.sh and its JSON comes back as is', async () => {
  const runs: string[][] = []
  const fake = (answer: ReturnType<typeof ran>) =>
    ({
      env: { get: async (name: string) => (name === 'HOME' ? '/home/me' : undefined) },
      fs: { stat: async (path: string) => (path === '/home/me/.config/cockpit/herdr/cockpit.sh' ? {} : Promise.reject(new Error('missing'))), read: async () => '{}' },
      process: {
        run: async (argv: string[]) => {
          runs.push(argv)
          return answer
        },
      },
    }) as unknown as Parameters<typeof find>[0]

  expect(await find(fake(ran(0, '{"entries":[]}\n')), { query: 'quota' }, searchArgs)).toEqual({ result: '{"entries":[]}' })
  expect(runs[0]).toEqual(['sh', '/home/me/.config/cockpit/herdr/cockpit.sh', 'search', 'quota'])
  expect(await find(fake(ran(1, '', 'cockpit events: the cockpit hub did not start')), { since: '2h' }, changesArgs)).toEqual({
    deny: 'cockpit events: the cockpit hub did not start',
  })
  expect(await find(fake(ran(0)), {}, changesArgs)).toEqual({ deny: 'cockpit events: no cockpit binary; run /cockpit:doctor' })
  expect(await find(fake(ran(0)), { app: 'todo' }, changesArgs)).toEqual({ deny: 'app: one of inbox, monitor, got "todo"' })
  expect(runs.length).toBe(3)
})

const envOf = (vars: Record<string, string>, files: Record<string, string> = {}) =>
  ({
    env: { get: async (name: string) => vars[name] },
    fs: {
      stat: async (path: string) => (path in files ? {} : Promise.reject(new Error('missing'))),
      read: async (path: string) => (path in files ? files[path] : Promise.reject(new Error('missing'))),
    },
  }) as unknown as Parameters<typeof home>[0]

test('the home is HOME, else the Windows profile with forward slashes, else an error', async () => {
  expect(await home(envOf({ HOME: '/home/me', USERPROFILE: 'C:\\Users\\me' }))).toBe('/home/me')
  expect(await home(envOf({ USERPROFILE: 'C:\\Users\\me' }))).toBe('C:/Users/me')
  expect(await home(envOf({ HOMEDRIVE: 'C:', HOMEPATH: '\\Users\\me' }))).toBe('C:/Users/me')
  await expect(home(envOf({}))).rejects.toThrow('no home folder')
})

test('a Windows binary noted by the app beats a linked cockpit.sh; a Unix one does not', async () => {
  const exe = 'C:/Users/me/AppData/Local/herdr-app/cockpit.exe'
  const note = { 'C:/Users/me/.config/herdr-app/cockpit.json': JSON.stringify({ bin: exe }), [exe]: '' }
  const sh = { 'C:/Users/me/.config/cockpit/herdr/cockpit.sh': '' }
  const win = { USERPROFILE: 'C:\\Users\\me' }
  expect(await cockpitCommand(envOf(win, { ...note, ...sh }))).toEqual([exe])
  expect(await cockpitCommand(envOf(win, sh))).toEqual(['sh', 'C:/Users/me/.config/cockpit/herdr/cockpit.sh'])
  const bin = '/home/me/.local/bin/cockpit'
  const unix = { '/home/me/.config/herdr-app/cockpit.json': JSON.stringify({ bin }), [bin]: '', '/home/me/.config/cockpit/herdr/cockpit.sh': '' }
  expect(await cockpitCommand(envOf({ HOME: '/home/me' }, unix))).toEqual(['sh', '/home/me/.config/cockpit/herdr/cockpit.sh'])
})
