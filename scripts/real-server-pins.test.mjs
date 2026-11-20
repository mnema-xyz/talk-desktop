import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

// The real-server jobs of the Windows/macOS and the Linux workflow run the same
// mnema-talk release: the pins must move together. The Windows and the macOS client
// workflows each carry the fixture version of the signed release they test.
const pin = (workflow, name) => readFileSync(new URL(`../.github/workflows/${workflow}`, import.meta.url), 'utf8').match(new RegExp(`^  ${name}: '?([^'\\s]+)`, 'm'))?.[1]

test('every real-server job pins the same server release, commit and SeaweedFS', () => {
  for (const name of ['REAL_SERVER_TAG', 'REAL_SERVER_REV', 'SEAWEEDFS_VERSION']) {
    const shared = pin('desktop-client-real-server.yml', name)
    assert.ok(shared, `${name} in desktop-client-real-server.yml`)
    assert.equal(pin('desktop-linux.yml', name), shared, name)
  }
  assert.match(pin('desktop-client-real-server.yml', 'REAL_SERVER_REV'), /^[0-9a-f]{40}$/)
})

test('the Windows and the macOS client workflows test the same published fixture release', () => {
  const windows = pin('desktop-client-windows.yml', 'FIXTURE_VERSION')
  assert.ok(windows, 'FIXTURE_VERSION in desktop-client-windows.yml')
  assert.equal(pin('desktop-client-macos.yml', 'FIXTURE_VERSION'), windows)
})
