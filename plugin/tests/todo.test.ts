import { expect, test } from 'claude-code/testing'

import { todoAdded, todoItems } from '../hooks/config'

test('/todo keeps a CRLF file in CRLF and an LF file in LF', () => {
  expect(todoAdded('# To-do\r\n\r\n- [ ] one\r\n- [x] two\r\n', 'new')).toBe('# To-do\r\n\r\n- [ ] new\r\n- [ ] one\r\n- [x] two\r\n')
  expect(todoAdded('# To-do\n\n- [ ] one\n', 'new')).toBe('# To-do\n\n- [ ] new\n- [ ] one\n')
  expect(todoItems('# To-do\r\n\r\n- [ ] one\r\n')).toEqual(['- [ ] one'])
})
