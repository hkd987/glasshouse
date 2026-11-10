// Where a suite keeps the state of the development-host glassd it starts.
//
// In the checkout's build folder, where its logs are easy to find, unless
// the checkout is so deep that an agent's socket wouldn't fit under it. A
// Unix socket's path stops at about 104 bytes (103 on the strictest system
// we build on), and the longest here is an agent's, in its task's sandbox.
// Past the limit every agent's task fails before it starts ("task socket:
// path must be shorter than SUN_LEN"): a CI runner's checkout did that to
// the tests that start such an agent, and nothing said a folder was too
// deep. Then the state goes in the temporary folder instead.
// (tools/dev/host-run.sh makes the same check, and says so.)

import { tmpdir, userInfo } from "node:os";
import { join } from "node:path";

const AGENT_SOCKET = "/state/sandboxes/glass-00000000/run/glasshouse/agent.sock";
const SOCKET_PATH_MOST = 103;

/** `inCheckout` if an agent's socket fits under it; else `<tmp>/glass-e2e-<uid>/<short>`. */
export function suiteFolder(inCheckout: string, short: string): string {
  if (Buffer.byteLength(inCheckout + AGENT_SOCKET) <= SOCKET_PATH_MOST) return inCheckout;
  return join(tmpdir(), `glass-e2e-${userInfo().uid}`, short);
}
