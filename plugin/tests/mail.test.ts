import { expect, test } from 'claude-code/testing'

import { mailAccounts } from '../hooks/config'
import { hubReads, isFresh, keyOf, messagesOf, threadFile, threadOf, unescape, withAccount } from '../hooks/mail'
import { checkRequest } from '../hooks/protocol'

// Claude's Gmail connector answers search_threads with threads and, in the summary view, their latest message.
const GMAIL = {
  enabled: true,
  name: 'Gmail',
  server: 'claude.ai Gmail',
  tools: {},
  fields: {
    messages: 'threads[].messages', id: 'threadId', threadId: 'threadId', from: 'sender', fromName: 'addressDisplayNames.0.displayName',
    subject: 'subject', snippet: 'snippet', url: 'viewUrl', date: 'date', unread: 'labelIds', labels: 'labelIds|label_ids',
    thread: 'messages', body: 'plaintextBody|plaintext_body', html: 'htmlBody|html_body',
  },
  urlTemplate: '',
}

test('unread Gmail threads read as the inbox mail, newest first', () => {
  const answer = {
    threads: [
      { id: 't1', messages: [{ threadId: 't1', sender: 'notifications@linear.app', addressDisplayNames: [{ displayName: 'Linear' }], subject: '6 unread',
        snippet: 'You have\n 6   unread', date: '2026-11-04T14:53:22Z', labelIds: ['UNREAD', 'INBOX'], viewUrl: 'https://mail.google.com/#all/t1' }] },
      { id: 't2', messages: [{ threadId: 't2', sender: 'Ana Lima <ana@acme.io>', subject: 'Invitation', snippet: 'Join', date: '2026-11-04T15:00:00Z',
        labelIds: ['INBOX'], viewUrl: 'https://mail.google.com/#all/t2' }] },
    ],
  }
  const messages = messagesOf(GMAIL, answer)
  expect(messages.map(m => m.id)).toEqual(['t2', 't1'])
  expect(messages[0]).toMatchObject({ from: 'ana@acme.io', fromName: 'Ana Lima', unread: false })
  expect(messages[1]).toMatchObject({ fromName: 'Linear', snippet: 'You have 6 unread', unread: true, url: 'https://mail.google.com/#all/t1' })
  expect(messages[1]?.labels).toEqual(['UNREAD', 'INBOX'])
})

test('a Gmail thread reads whole in either case, oldest first, as the mail space keeps it', () => {
  // get_thread with FULL_CONTENT: camelCase as the connector answers, snake_case as it documents its fields.
  const answer = {
    id: 't2',
    messages: [
      { id: 'm2', sender: 'Ana Lima <ana@acme.io>', date: '2026-11-04T15:00:00Z', htmlBody: '<p>Noon?</p>', plaintextBody: 'Noon?' },
      { id: 'm1', sender: 'me@acme.example', date: '2026-11-04T14:00:00Z', plaintext_body: 'Lunch?', html_body: '' },
    ],
  }
  const thread = threadOf(GMAIL, answer)
  expect(thread.map(m => m.id)).toEqual(['m1', 'm2'])
  expect(thread[0]).toMatchObject({ from: 'me@acme.example', fromName: 'me@acme.example', text: 'Lunch?', html: '' })
  expect(thread[1]).toMatchObject({ fromName: 'Ana Lima', html: '<p>Noon?</p>', text: 'Noon?' })
  expect(threadOf({ ...GMAIL, fields: {} }, { from: 'Sam <sam@example.com>', body: 'Hi' })).toMatchObject([{ fromName: 'Sam', text: 'Hi' }])
  expect(threadFile('../a/b:c')).toBe('_a_b_c.json')
})

test('the mail space asks a session for a thread with a mail-get request', () => {
  expect(checkRequest({ type: 'mail-get', label: 'Invitation', message: 't2' })).toEqual({ request: { type: 'mail-get', label: 'Invitation', message: 't2' } })
  expect(checkRequest({ type: 'mail-get', label: 'Invitation' })).toMatchObject({ refusal: expect.stringContaining('mail-get needs message as a string') })
})

test('a kept thread is read again when a reply lands after it, or when reading it failed', () => {
  const date = '2026-11-04T10:00:00Z'
  const at = Date.parse(date)
  expect(isFresh({ at: at + 1 }, { date })).toBe(true)
  expect(isFresh({ at: at - 1 }, { date })).toBe(false)
  expect(isFresh({ at: at + 1, error: 'Gmail timed out' }, { date })).toBe(false)
  expect(isFresh(null, { date })).toBe(false)
})

test('escaped snippets and subjects read as text', () => {
  expect(unescape('haven&#39;t &amp; R&amp;D &lt;3 &#x2019; &bogus;')).toBe("haven't & R&D <3 \u2019 &bogus;")
})

test('a mail block without accounts is the one account default; listed accounts carry their own mapping', () => {
  expect(mailAccounts(GMAIL)).toEqual([{ id: 'default', name: 'Gmail', server: 'claude.ai Gmail', tools: {}, fields: GMAIL.fields, urlTemplate: '' }])
  const accounts = mailAccounts({ ...GMAIL, accounts: [{ id: 'work', name: 'Acme', server: 'imap-acme' }, { id: 'home', server: 'imap-home', enabled: false }, { id: '' }] })
  expect(accounts).toEqual([{ id: 'work', name: 'Acme', server: 'imap-acme', tools: {}, fields: {}, urlTemplate: '' }])
})

test('a session writes its account and leaves the others, keyed apart when ids clash', () => {
  const message = (id: string, date: string, unread = true) => ({ id, threadId: id, from: 'maya@example.com', fromName: 'Maya', subject: id, snippet: '', url: '', date, unread, labels: [] })
  const hub = { ...message('m1', '2026-11-04T10:00:00Z'), account: 'work' }
  const state = { at: 5, by: 'hub' as const, messages: [hub], notified: ['work:m1'], accounts: [{ id: 'work', name: 'Acme', by: 'hub' as const, at: 5 }] }
  const after = withAccount(state, { id: 'gmail', name: 'Gmail', server: 'claude.ai Gmail', tools: {}, fields: {}, urlTemplate: '' }, [message('m1', '2026-11-04T11:00:00Z')], 9)
  expect(after.messages.map(m => keyOf(m.account, m.id))).toEqual(['gmail:m1', 'work:m1'])
  expect(after.notified).toEqual(['gmail:m1', 'work:m1'])
  expect(after.accounts?.map(a => [a.id, a.by])).toEqual([['work', 'hub'], ['gmail', 'session']])
  expect(keyOf('default', 'm1')).toBe('m1')
  expect(threadFile(keyOf('work', 'INBOX:7:42'))).toBe('work_INBOX_7_42.json')
})

test('the hub reading an account keeps the session off it, per account', () => {
  const now = 1_000_000
  const state = { at: now, by: 'hub' as const, messages: [], notified: [], accounts: [{ id: 'work', name: 'Acme', by: 'hub' as const, at: now - 1000 }, { id: 'gmail', name: 'Gmail', by: 'session' as const, at: now }] }
  expect(hubReads(state, 'work', now)).toBe(true)
  expect(hubReads(state, 'gmail', now)).toBe(false)
  expect(hubReads({ at: now, by: 'hub', messages: [], notified: [] }, 'default', now)).toBe(true)
  expect(hubReads({ at: now - 10 * 60_000, by: 'hub', messages: [], notified: [] }, 'default', now)).toBe(false)
  expect(hubReads(null, 'default', now)).toBe(false)
})
