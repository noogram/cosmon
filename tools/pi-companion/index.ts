// SPDX-License-Identifier: AGPL-3.0-only
/** Native Pi entry point, deliberately free of startup processes and model tools. */
import type { ExtensionAPI } from '@earendil-works/pi-coding-agent';
import { truncateToWidth, matchesKey, Key } from '@earendil-works/pi-tui';
import { register } from './extension.mjs';

export default function (pi: ExtensionAPI) {
  register(pi, { truncateToWidth, matchesKey, Key });
}
