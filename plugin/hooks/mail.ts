import type { EngineInterface } from 'claude-code'

import { isMcpNotReady, notify, shortError } from './ci'
import { cacheDir, loadConfig, mailAccounts, writeAtomic } from './config'
import type { Mail, MailAccount } from './config'
import { HUB_FRESH_MS } from './feed'
import { callMapped, listAt, pick, text } from './platform'
import { withLease, writeDoc } from './store'

type $ = EngineInterface

// Unread mail through Claude Code's mail connectors, for the mail accounts whose server the hub does not reach itself:
// their messages in the same `mail` document the hub writes (crates/connectors mail.rs), refreshed by one session, the
// other accounts' left as they are, and the threads read whole ahead of time (or when the mail space asks), kept a week
// in mail/<key>.json.

const STALE_MS = 2 * 60_000
const MAX_MESSAGES = 50
const MAX_SNIPPET = 200
const MAX_HTML = 400_000
const KEEP_THREADS_MS = 7 * 86_400_000
// How many threads one refresh reads ahead, so a full inbox fills over a few refreshes.
const READ_AHEAD = 10

export type MailMessage = {
  id: string
  threadId: string
  from: string
  fromName: string
  subject: string
  snippet: string
  url: string
  date: string
  unread: boolean
  labels: string[]
  // The account it came from, unless it is the implied one.
  account?: string
}

export type ThreadMessage = { id: string; from: string; fromName: string; date: string; html: string; text: string }
type AccountState = { id: string; name: string; by: 'hub' | 'session'; at: number; error?: string }
type MailState = { at: number; messages: MailMessage[]; notified: string[]; error?: string; by?: 'hub'; accounts?: AccountState[] }

const DEFAULT_ACCOUNT = 'default'
const accountOf = (m: Pick<MailMessage, 'account'>) => m.account || DEFAULT_ACCOUNT

// A message's key across accounts, as ids may clash between mailboxes: its id in the implied account, else
// `<account>:<id>` (the hub's mail::key).
export const keyOf = (account: string | undefined, id: string) => (!account || account === DEFAULT_ACCOUNT ? id : `${account}:${id}`)
const messageKey = (m: Pick<MailMessage, 'account' | 'id'>) => keyOf(m.account, m.id)

// Whether the hub reads this account itself, as the document says: then this session leaves it alone.
export const hubReads = (state: MailState | null, account: string, now = Date.now()) => {
  if (!state) return false
  const entry = state.accounts?.find(a => a.id === account)
  if (entry) return entry.by === 'hub' && now - entry.at < HUB_FRESH_MS
  return account === DEFAULT_ACCOUNT && !state.accounts?.length && state.by === 'hub' && now - state.at < HUB_FRESH_MS
}

// The document with one account's messages in place of its old ones, the other accounts' kept as they are.
export function withAccount(state: MailState | null, account: MailAccount, messages: MailMessage[], now: number): MailState {
  const tagged = account.id === DEFAULT_ACCOUNT ? messages : messages.map(m => ({ ...m, account: account.id }))
  const others = (state?.messages ?? []).filter(m => accountOf(m) !== account.id)
  const all = [...others, ...tagged].sort((a, b) => (Date.parse(b.date) || 0) - (Date.parse(a.date) || 0))
  const unread = tagged.filter(m => m.unread).map(messageKey)
  const notified = [...new Set([...unread, ...(state?.notified ?? [])])].slice(0, 300)
  const accounts = [...(state?.accounts ?? []).filter(a => a.id !== account.id), { id: account.id, name: account.name, by: 'session' as const, at: now }]
  const { error: _, ...rest } = state ?? { at: 0, messages: [], notified: [] }
  return { ...rest, at: now, messages: all, notified, accounts }
}

async function readMail($: $): Promise<MailState | null> {
  try {
    return JSON.parse(await $.fs.read(`${await cacheDir($)}/mail.json`)) as MailState
  } catch {
    return null
  }
}

// "Ana Lima <ana@acme.com>" as its address and name.
function sender(from: string): [string, string | undefined] {
  const m = from.match(/^\s*"?([^"<]*?)"?\s*<([^>]+)>\s*$/)
  return m ? [m[2]!.trim(), m[1]!.trim() || undefined] : [from.trim(), undefined]
}

// A snippet or subject as the mail server escapes it for HTML (`haven&#39;t`, `R&amp;D`), as plain text.
const ENTITIES: Record<string, string> = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' }
export const unescape = (text: string) =>
  text.replace(/&(#x[0-9a-f]+|#\d+|[a-z]+);/gi, (whole, name: string) => {
    if (name[0] !== '#') return ENTITIES[name.toLowerCase()] ?? whole
    const code = name[1] === 'x' || name[1] === 'X' ? parseInt(name.slice(2), 16) : parseInt(name.slice(1), 10)
    return Number.isFinite(code) && code <= 0x10ffff ? String.fromCodePoint(code) : whole
  })

const isoDate = (date: string) => (Number.isNaN(Date.parse(date)) ? date : new Date(date).toISOString().replace(/\.\d{3}Z$/, 'Z'))

const isUnread = (v: unknown) =>
  Array.isArray(v) ? v.some(l => String(l).toUpperCase() === 'UNREAD') : v === true || v === 1 || ['true', 'unread', 'yes', '1'].includes(String(v).toLowerCase())

const listOf = (v: unknown) => (Array.isArray(v) ? v.filter((l): l is string => typeof l === 'string') : [])

export function messagesOf(mail: Pick<Mail, 'fields' | 'urlTemplate'>, answer: unknown): MailMessage[] {
  const f = mail.fields
  const out: MailMessage[] = []
  for (const raw of listAt(answer, f.messages)) {
    const id = text(pick(raw, f.id))
    if (!id) continue
    const threadId = text(pick(raw, f.threadId)) ?? ''
    const [address, named] = sender(text(pick(raw, f.from)) ?? '')
    const date = text(pick(raw, f.date)) ?? ''
    out.push({
      id,
      threadId,
      from: address,
      fromName: text(pick(raw, f.fromName)) || named || address,
      subject: unescape(text(pick(raw, f.subject)) ?? ''),
      snippet: unescape(text(pick(raw, f.snippet)) ?? '').split(/\s+/).join(' ').trim().slice(0, MAX_SNIPPET),
      url: text(pick(raw, f.url)) || mail.urlTemplate.replace('{id}', id).replace('{threadId}', threadId),
      date: isoDate(date),
      unread: !f.unread || isUnread(pick(raw, f.unread)),
      labels: listOf(pick(raw, f.labels)),
    })
  }
  return out.sort((a, b) => (Date.parse(b.date) || 0) - (Date.parse(a.date) || 0)).slice(0, MAX_MESSAGES)
}

let inflight: Promise<void> | undefined

// The account a request or message names, as the config maps it.
async function accountNamed($: $, id: string | undefined) {
  const { mail } = await loadConfig($)
  const account = mailAccounts(mail).find(a => a.id === (id || DEFAULT_ACCOUNT))
  if (!account) throw new Error(`no mail account ${id} in the config`)
  return account
}

export function refreshMail($: $, isForced = false) {
  inflight ??= (async () => {
    try {
      const config = await loadConfig($)
      if (!config.mail.enabled) return
      const every = Math.max(15, Number(config.refresh?.mail ?? 60)) * 1000
      // An account whose server the config lists for cockpit itself is the hub's to read.
      const servers = (config as { servers?: Record<string, unknown> }).servers ?? {}
      const accounts = mailAccounts(config.mail).filter(a => a.server && !servers[a.server] && a.tools.listInbox?.tool)
      if (!accounts.length) return
      await withLease($, 'mail', async () => {
        for (const account of accounts) {
          const prev = await readMail($)
          // The hub keeps an account's messages while it reaches that account's server itself.
          if (hubReads(prev, account.id)) continue
          const at = prev?.accounts?.find(a => a.id === account.id)?.at ?? (prev?.accounts?.length ? 0 : prev?.at ?? 0)
          if (!isForced && prev && Date.now() - at < Math.min(every, STALE_MS)) continue
          await refreshAccount($, account, prev)
        }
      })
    } finally {
      inflight = undefined
    }
  })()
  return inflight
}

async function refreshAccount($: $, account: MailAccount, prev: MailState | null) {
  const messages = await callMapped($, account, 'listInbox')
    .then(answer => messagesOf(account, answer))
    .catch(async (err: Error) => {
      if (!isMcpNotReady(err)) {
        const base: MailState = (await readMail($)) ?? { messages: [], notified: [], at: 0 }
        const error = `${account.name} ${shortError(err)}`
        const at = base.accounts?.find(a => a.id === account.id)?.at ?? 0
        const accounts = [...(base.accounts ?? []).filter(a => a.id !== account.id), { id: account.id, name: account.name, by: 'session' as const, at, error }]
        // A single implied account keeps the document as it always was: no accounts list.
        await writeDoc($, 'mail', { ...base, error, accounts: base.accounts || account.id !== DEFAULT_ACCOUNT ? accounts : undefined })
      }
      return null
    })
  if (!messages) return
  const tagged = account.id === DEFAULT_ACCOUNT ? messages : messages.map(m => ({ ...m, account: account.id }))
  const known = new Set(prev?.notified ?? [])
  // Only mail newer than any listed before in this account: a longer listing brings older unread mail in, which is not news.
  const before = (prev?.messages ?? []).filter(m => accountOf(m) === account.id)
  const newest = Math.max(0, ...before.map(m => Date.parse(m.date) || 0))
  const seen = prev && (prev.accounts?.find(a => a.id === account.id)?.at ?? (account.id === DEFAULT_ACCOUNT && !prev.accounts?.length ? prev.at : 0)) > 0
  const fresh = seen ? tagged.filter(m => m.unread && !known.has(messageKey(m)) && (Date.parse(m.date) || 0) > newest) : []
  // Read again just before writing, so another account's messages written meanwhile stay.
  const state = withAccount(await readMail($), account, messages, Date.now())
  // A single implied account keeps the document as it always was: no accounts list.
  await writeDoc($, 'mail', account.id === DEFAULT_ACCOUNT && state.accounts?.length === 1 ? { ...state, accounts: undefined } : state)
  const where = (state.accounts?.length ?? 0) > 1 ? ` · ${account.name}` : ''
  if (fresh.length > 3) await notify($, `✉️ ${fresh.length} new mails${where}`, fresh.slice(0, 3).map(m => `${m.fromName}: ${m.subject}`).join('\n'), 'space:mail')
  else for (const m of fresh) await notify($, `✉️ ${m.fromName}${where}`, m.subject || m.snippet, `mail:${messageKey(m)}`)
  await readAhead($, account, tagged)
}

// A mail request from the mail space (archive, mark read) through the account's connector, and the message gone or
// read in mail.json.
export async function mailRequest($: $, kind: 'mail-archive' | 'mail-read', id: string, accountId?: string) {
  const account = await accountNamed($, accountId)
  const state = await readMail($)
  const key = keyOf(account.id, id)
  const message = state?.messages.find(m => messageKey(m) === key)
  const values = { message: id, thread: message?.threadId || id }
  await callMapped($, account, kind === 'mail-archive' ? 'archive' : 'markRead', values)
  const now = await readMail($)
  if (!now) return
  const messages =
    kind === 'mail-archive' ? now.messages.filter(m => messageKey(m) !== key) : now.messages.map(m => (messageKey(m) === key ? { ...m, unread: false } : m))
  await writeDoc($, 'mail', { ...now, messages })
}

// A `get` answer as the thread, oldest first: the messages at the `thread` field, else the answer as one message.
export function threadOf(mail: Pick<Mail, 'fields'>, answer: unknown): ThreadMessage[] {
  const f = mail.fields
  const listed = f.thread ? listAt(answer, f.thread) : []
  const raws = listed.length ? listed : [answer]
  return raws
    .map(raw => {
      const [address, named] = sender(text(pick(raw, f.from ?? 'from')) ?? '')
      return {
        id: text(pick(raw, 'id')) ?? '',
        from: address,
        fromName: text(pick(raw, f.fromName)) || named || address,
        date: isoDate(text(pick(raw, f.date ?? 'date')) ?? ''),
        html: (text(pick(raw, f.html ?? 'html')) ?? '').slice(0, MAX_HTML),
        text: text(pick(raw, f.body ?? 'body')) ?? '',
      }
    })
    .filter(m => m.html || m.text || m.from)
    .sort((a, b) => (Date.parse(a.date) || 0) - (Date.parse(b.date) || 0))
}

// Whether a kept thread holds the message: read without error since its date, as a reply landing makes it stale.
export const isFresh = (doc: { at: number; error?: string } | null, m: Pick<MailMessage, 'date'>) => Boolean(doc && !doc.error && doc.at >= (Date.parse(m.date) || 0))

async function keptFresh($: $, m: MailMessage) {
  const doc = await $.fs.read(`${await cacheDir($)}/mail/${threadFile(messageKey(m))}`).then(t => JSON.parse(t) as { at: number; error?: string }).catch(() => null)
  return isFresh(doc, m)
}

// The threads of the listed messages read ahead, so opening one is instant: the newest first, a few per refresh.
async function readAhead($: $, account: MailAccount, messages: MailMessage[]) {
  if (!account.tools.get?.tool) return
  let read = 0
  for (const m of messages) {
    if (read >= READ_AHEAD) return
    if (await keptFresh($, m)) continue
    read++
    await mailGet($, m.id, m.account).catch(() => undefined)
  }
}

// The file a message's thread is kept in, by the message's key, safe whatever it holds; the hub names it the same
// (connectors mail.rs).
export const threadFile = (key: string) => `${key.replace(/[^A-Za-z0-9._-]/g, '_').replace(/^\.+/, '')}.json`

// A thread the mail space asked for, read whole through the account's connector into mail/<key>.json, where the space
// picks it up; a failure is kept there too, so the space says why. Threads older than a week go.
export async function mailGet($: $, id: string, accountId?: string) {
  const account = await accountNamed($, accountId)
  const dir = `${await cacheDir($)}/mail`
  await $.process.run(['mkdir', '-p', dir])
  for (const entry of await $.fs.list(dir).catch(() => [])) {
    if (entry.kind === 'file' && Date.now() - entry.mtimeMs > KEEP_THREADS_MS) await $.process.run(['rm', '-f', `${dir}/${entry.name}`])
  }
  const key = keyOf(account.id, id)
  const listed = (await readMail($))?.messages.find(m => messageKey(m) === key)
  const path = `${dir}/${threadFile(key)}`
  try {
    const answer = await callMapped($, account, 'get', { message: id, thread: listed?.threadId || id })
    await writeAtomic($, path, JSON.stringify({ at: Date.now(), id: key, messages: threadOf(account, answer) }))
  } catch (err) {
    await writeAtomic($, path, JSON.stringify({ at: Date.now(), id: key, messages: [], error: `${account.name} ${shortError(err as Error)}` }))
    throw err
  }
}
