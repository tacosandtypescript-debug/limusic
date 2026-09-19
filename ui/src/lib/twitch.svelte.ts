// Reactive Twitch session state, driven by the Rust `tw-state` event.
//
// Mirrors `lt.svelte.ts`: the session lives in Rust and this is a read-only projection of it, kept
// in one place so the settings panel and anything later (a queue panel badge, a titlebar
// indicator) read the same values. Mutations never happen here — they go through the `tw*`
// commands in `api.ts`, and the result comes back as an event.
import type { TwitchSnapshot } from './api';

/**
 * The state before the first snapshot arrives, and the shape the rest of the UI can rely on.
 *
 * `phase: 'disconnected'` rather than a loading state: on a cold start with nothing stored, that is
 * the truth, and it keeps the panel from rendering a spinner that resolves into the same thing a
 * moment later.
 */
export const twitch = $state<TwitchSnapshot>({
	phase: 'disconnected',
	clientId: '',
	clientIdBundled: false,
	configured: false,
	device: null,
	account: null,
	channel: null,
	channelPointsAvailable: false,
	config: {
		version: 1,
		channelLogin: null,
		channelId: null,
		autoConnect: false,
		// Phase 3, inert until switched on — mirrors `TwitchConfig::default()` in Rust. A mismatch
		// here is a panel that renders before the first snapshot arrives and shows the wrong thing
		// for one frame, which is exactly the kind of thing nobody reports as a bug.
		requestsEnabled: false,
		commandPrefix: '!',
		requestAliases: ['sr', 'songrequest', 'request'],
		minRole: 'everyone',
		userCooldownSecs: 30,
		globalCooldownSecs: 5,
		rewardId: '',
		replyInChat: true
	},
	scopes: [],
	expiresAt: 0,
	validatedAt: 0,
	error: null,
	eventsub: {
		status: 'off',
		sessionId: null,
		subscriptionId: null,
		connectedAt: 0,
		messages: 0,
		duplicates: 0,
		error: null,
		recent: []
	}
});

/** Replace the reactive state from a fresh `tw-state` snapshot. */
export function applyTwitchState(s: TwitchSnapshot) {
	Object.assign(twitch, s);
}
