<script lang="ts">
	// The Twitch tab. Phase 1 of the integration: connect the streamer's own account, pick the
	// channel the bot will listen to, and show what was granted. Chat commands, Channel Points
	// rewards and Bits come in later phases and land in this same panel.
	//
	// Everything shown here comes from the `twitch` store, which mirrors the Rust `tw-state` event;
	// nothing is fetched here and nothing is inferred. In particular the permission list is the
	// list Rust got back from Twitch's `/validate`, not the list we asked for — those differ
	// whenever a user unticks a box on the consent screen, and showing what we asked for would be
	// a lie.
	//
	// Layout follows `DiscordSettings.svelte`, including its locally-defined GROUP/LABEL/CARD
	// strings: the settings dialog keeps those as locals rather than passing them down, and a
	// shared constant would be a bigger change to the core than this feature is worth.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import {
		Cancel01Icon,
		Copy01Icon,
		Logout01Icon,
		RefreshIcon,
		Tick02Icon
	} from '@hugeicons/core-free-icons';
	import { untrack } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Switch } from '$lib/components/ui/switch';
	import { Alert, AlertDescription } from '$lib/components/ui/alert';
	import * as api from '$lib/api';
	import type { TwitchChatMessage } from '$lib/api';
	import { copyText } from '$lib/clipboard';
	import { toast } from '$lib/player.svelte';
	import { twitch } from '$lib/twitch.svelte';
	import { t } from '$lib/i18n.svelte';

	const GROUP = 'mb-7 last:mb-1';
	const LABEL =
		'mb-2 px-1 text-[11px] font-semibold uppercase tracking-[0.08em] text-muted-foreground';
	const CARD = 'divide-y divide-border/60 overflow-hidden rounded-xl border bg-card';
	const ROW = 'flex items-start justify-between gap-4 px-4 py-3';


	// --- Phase 3: song requests ---------------------------------------------------------------
	//
	// Seeded once from the store, untracked, then owned here — the same rule the fields above
	// follow, and for the same reason: a `tw-state` event lands on connect, on validate and on
	// every save, and an effect that copied the store back into these would overwrite whatever the
	// user was halfway through typing.
	const ROLES = ['everyone', 'subscriber', 'vip', 'moderator', 'broadcaster'] as const;

	/** The stored aliases as one editable string, which is how they are shown: `!sr !songrequest`. */
	function aliasesToInput(prefix: string, aliases: string[]): string {
		return aliases.map((a) => `${prefix}${a}`).join(' ');
	}

	let requestsEnabled = $state(untrack(() => twitch.config.requestsEnabled ?? false));
	let commandInput = $state(
		untrack(() =>
			aliasesToInput(
				twitch.config.commandPrefix ?? '!',
				twitch.config.requestAliases ?? ['sr', 'songrequest']
			)
		)
	);
	let minRole = $state(untrack(() => twitch.config.minRole ?? 'everyone'));
	let userCooldown = $state(untrack(() => twitch.config.userCooldownSecs ?? 30));
	let globalCooldown = $state(untrack(() => twitch.config.globalCooldownSecs ?? 5));
	let replyInChat = $state(untrack(() => twitch.config.replyInChat ?? true));
	let rewardId = $state(untrack(() => twitch.config.rewardId ?? ''));

	let rewards = $state<api.TwitchReward[]>([]);
	let rewardsError = $state<string | null>(null);
	let rewardBusy = $state(false);
	let requestsError = $state<string | null>(null);

	/**
	 * Load the channel's rewards.
	 *
	 * The usual failure is a 403 for a channel that is not an affiliate or partner, which is not
	 * retryable and is worth showing as-is: a picker that silently lists nothing leaves the
	 * streamer hunting for a reward they cannot have.
	 */
	async function loadRewards() {
		rewardBusy = true;
		rewardsError = null;
		try {
			rewards = await api.twRewards();
		} catch (e) {
			rewards = [];
			rewardsError = String(e);
		} finally {
			rewardBusy = false;
		}
	}

	// Once, when there is a channel to ask about. Guarded on the array being empty so a save — which
	// emits a new snapshot — does not refetch on every keystroke elsewhere.
	$effect(() => {
		if (connected && twitch.channel && rewards.length === 0 && !rewardBusy && !rewardsError) {
			void loadRewards();
		}
	});

	/**
	 * Split the command box into a prefix and its names.
	 *
	 * The user types `!sr !songrequest`, which is what the panel advertises and what the config
	 * cannot store — it keeps a prefix and a list of bare names. Splitting here rather than asking
	 * for two fields keeps the common case (`!`) out of sight.
	 */
	function parseCommands(raw: string): { prefix: string; aliases: string[] } {
		const parts = raw.trim().split(/\s+/).filter(Boolean);
		if (parts.length === 0) return { prefix: '', aliases: [] };
		// The prefix is whatever the first token starts with that is not a letter or a digit, or `!`.
		const first = parts[0];
		const match = /^[^\p{L}\p{N}]+/u.exec(first);
		const prefix = match ? match[0] : '!';
		const aliases = parts.map((p) => p.slice(prefix.length)).filter(Boolean);
		return { prefix, aliases };
	}

	async function saveRequests() {
		busy = true;
		requestsError = null;
		const { prefix, aliases } = parseCommands(commandInput);
		try {
			await api.twSetRequests({
				enabled: requestsEnabled,
				prefix,
				aliases,
				minRole,
				userCooldownSecs: Number(userCooldown) || 0,
				globalCooldownSecs: Number(globalCooldown) || 0,
				rewardId,
				replyInChat
			});
			toast.success(t('settings.twitch.requests_saved'));
		} catch (e) {
			// Shown in place rather than as a toast: the messages name the field that was wrong
			// ("`moderater` is not a role"), and that is only useful next to the field.
			requestsError = String(e);
		} finally {
			busy = false;
		}
	}

	// --- derived state -----------------------------------------------------------------------

	const connecting = $derived(twitch.phase === 'connecting');
	const connected = $derived(twitch.phase === 'connected');
	const device = $derived(twitch.device);

	// --- local input state -------------------------------------------------------------------
	// Seeded **once** from the store, untracked, then owned here — the same thing
	// `DiscordSettings.svelte` does with its config blob. A plain `$effect` that copied the store
	// into these on every change would overwrite what the user is halfway through typing the
	// moment any `tw-state` event landed, which happens on connect, on validate and on save.
	let clientIdInput = $state(untrack(() => twitch.clientId));
	let channelInput = $state(untrack(() => twitch.config.channelLogin ?? ''));
	let busy = $state(false);
	// A `$state` copy of the clock, so the countdown below re-renders on its own.
	let nowMs = $state(Date.now());

	// Only while a code is on screen: a timer that runs for the life of the app to drive a label
	// nobody is looking at is pure waste. The `$effect` cleanup is what stops it.
	$effect(() => {
		if (!device) return;
		const id = setInterval(() => (nowMs = Date.now()), 1000);
		return () => clearInterval(id);
	});

	const secondsLeft = $derived(
		device ? Math.max(0, device.expiresAt - Math.floor(nowMs / 1000)) : 0
	);
	const minutesLeft = $derived(Math.ceil(secondsLeft / 60));

	// --- chat (EventSub) ---------------------------------------------------------------------

	/**
	 * Badge sets that mean something to a viewer, mapped to the short chip shown next to the name.
	 *
	 * Read from the badge list rather than from a role of our own: this is exactly what the chat
	 * message carries, and inventing a hierarchy here would be the start of a second permissions
	 * model competing with the one phase 3 will add.
	 */
	const ROLE_LABELS: Record<string, string> = {
		broadcaster: 'HOST',
		moderator: 'MOD',
		vip: 'VIP',
		subscriber: 'SUB',
		founder: 'SUB',
		premium: 'PRIME',
		staff: 'STAFF',
		admin: 'ADMIN',
		partner: 'PARTNER'
	};

	/** The distinct roles a message's badges imply, in the order they first appear. */
	function roles(m: TwitchChatMessage): string[] {
		const out: string[] = [];
		for (const b of m.badges) {
			const label = ROLE_LABELS[b.setId];
			if (label && !out.includes(label)) out.push(label);
		}
		return out;
	}

	const chat = $derived(twitch.eventsub);
	const chatReady = $derived(connected && !!twitch.config.channelLogin);

	const chatLabel = $derived(
		chat.status === 'live'
			? t('settings.twitch.chat_live', { channel: twitch.config.channelLogin ?? '' })
			: chat.status === 'connecting'
				? t('settings.twitch.chat_connecting')
				: chat.status === 'retrying'
					? t('settings.twitch.chat_retrying')
					: chat.status === 'failed'
						? t('settings.twitch.chat_failed')
						: t('settings.twitch.chat_off')
	);

	/** Just the tail, not the whole id — it is a debugging handle, not something to read. */
	const sessionShort = $derived(
		chat.sessionId ? `${chat.sessionId.slice(0, 8)}…` : null
	);

	// --- formatting --------------------------------------------------------------------------

	/** Unix seconds → a local time, or an em dash when we have nothing. */
	function clock(secs: number): string {
		if (!secs) return '—';
		return new Date(secs * 1000).toLocaleTimeString(undefined, {
			hour: '2-digit',
			minute: '2-digit'
		});
	}

	// --- actions -----------------------------------------------------------------------------
	// Every one of these reports failures as a toast and leaves the panel to the store: the Rust
	// side owns the session, and a second copy of "am I connected" here is how the two drift.

	async function run(fn: () => Promise<unknown>, ok?: string) {
		busy = true;
		try {
			await fn();
			if (ok) toast.success(ok);
		} catch (e) {
			toast.error(String(e));
		} finally {
			busy = false;
		}
	}

	const saveClientId = () => run(() => api.twSetClientId(clientIdInput), t('settings.twitch.saved'));
	const connect = () => run(api.twConnect);
	const cancel = () => run(api.twCancel);
	const disconnect = () =>
		run(api.twDisconnect, t('settings.twitch.disconnected'));
	const saveChannel = () =>
		run(
			() => api.twSetChannel(channelInput),
			channelInput.trim()
				? t('settings.twitch.channel_set', { name: channelInput.trim() })
				: undefined
		);
	const clearChannel = () => {
		channelInput = '';
		return run(() => api.twSetChannel(''));
	};
</script>

<section class={GROUP}>
	<h3 class={LABEL}>{t('settings.sections.activity')}</h3>
	<div class={CARD}>
		<div class={ROW}>
			<div class="min-w-0">
				<p class="text-sm font-medium">
					{#if connected && twitch.account}
						{t('settings.twitch.connected_as', { name: twitch.account.displayName })}
					{:else if connecting}
						{t('settings.twitch.connecting')}
					{:else}
						{t('settings.twitch.connect')}
					{/if}
				</p>
				<p class="mt-0.5 text-xs text-muted-foreground">
					{#if connected}
						{twitch.validatedAt
							? t('settings.twitch.validated', { when: clock(twitch.validatedAt) })
							: t('settings.twitch.not_validated')}
					{:else}
						{t('settings.twitch.intro')}
					{/if}
				</p>
			</div>
			<div class="flex shrink-0 items-center gap-2">
				{#if connected}
					<Button variant="outline" size="sm" disabled={busy} onclick={disconnect}>
						<HugeiconsIcon icon={Logout01Icon} class="mr-1.5 h-3.5 w-3.5" />
						{t('settings.twitch.disconnect')}
					</Button>
				{:else if connecting}
					<Button variant="outline" size="sm" disabled={busy} onclick={cancel}>
						<HugeiconsIcon icon={Cancel01Icon} class="mr-1.5 h-3.5 w-3.5" />
						{t('common.cancel')}
					</Button>
				{:else}
					<Button size="sm" disabled={!twitch.configured || busy} onclick={connect}>
						{t('settings.twitch.connect')}
					</Button>
				{/if}
			</div>
		</div>

		{#if !twitch.configured}
			<div class="px-4 py-3">
				<Alert>
					<AlertDescription>{t('settings.twitch.needs_client_id')}</AlertDescription>
				</Alert>
			</div>
		{/if}

		{#if twitch.error}
			<div class="px-4 py-3">
				<Alert variant="destructive">
					<AlertDescription>{twitch.error}</AlertDescription>
				</Alert>
			</div>
		{/if}

		{#if device}
			<!-- The one thing a user has to act on, so it gets its own block rather than a row. -->
			<div class="px-4 py-3">
				<p class="text-sm text-muted-foreground">{t('settings.twitch.device_instruction')}</p>
				<div class="mt-2 flex items-center gap-2">
					<code
						class="rounded-lg border bg-muted/60 px-3 py-1.5 font-mono text-lg font-semibold tracking-widest"
						>{device.userCode}</code
					>
					<Button
						variant="ghost"
						size="icon"
						title={t('common.copy')}
						onclick={() => copyText(device.userCode).catch((e) => toast.error(String(e)))}
					>
						<HugeiconsIcon icon={Copy01Icon} class="h-4 w-4" />
					</Button>
				</div>
				<div class="mt-3 flex flex-wrap items-center gap-2">
					<Button
						size="sm"
						onclick={() => run(() => api.openExternal(device.verificationUri))}
					>
						{t('settings.twitch.device_open')}
					</Button>
					<span class="text-xs text-muted-foreground">
						{t('settings.twitch.device_expires', { minutes: minutesLeft })}
					</span>
				</div>
				<p class="mt-2 text-xs text-muted-foreground">{t('settings.twitch.device_hint')}</p>
			</div>
		{/if}
	</div>
</section>

<section class={GROUP}>
	<h3 class={LABEL}>{t('settings.sections.system')}</h3>
	<div class={CARD}>
		<div class="px-4 py-3">
			<p class="text-sm font-medium">{t('settings.twitch.client_id')}</p>
			<p class="mt-0.5 text-xs text-muted-foreground">
				{twitch.clientIdBundled
					? t('settings.twitch.client_id_bundled')
					: t('settings.twitch.client_id_hint')}
			</p>
			<div class="mt-2.5 flex items-center gap-2">
				<Input
					bind:value={clientIdInput}
					placeholder={t('settings.twitch.client_id_placeholder')}
					class="font-mono text-xs"
					spellcheck={false}
					autocomplete="off"
				/>
				<Button
					variant="outline"
					size="sm"
					disabled={busy || clientIdInput.trim() === twitch.clientId}
					onclick={saveClientId}
				>
					{t('common.save')}
				</Button>
			</div>
			<button
				type="button"
				class="mt-2 cursor-pointer text-xs text-primary underline-offset-4 hover:underline"
				onclick={() => run(() => api.openExternal('https://dev.twitch.tv/console/apps'))}
			>
				{t('settings.twitch.register')}
			</button>
		</div>
	</div>
</section>

{#if connected}
	<section class={GROUP}>
		<h3 class={LABEL}>{t('settings.twitch.channel')}</h3>
		<div class={CARD}>
			<div class="px-4 py-3">
				<p class="text-sm font-medium">
					{#if twitch.channel}
						{t('settings.twitch.channel_watching', {
							name: twitch.channel.displayName,
							id: twitch.channel.id
						})}
					{:else}
						{t('settings.twitch.channel_none')}
					{/if}
				</p>
				<p class="mt-0.5 text-xs text-muted-foreground">
					{t('settings.twitch.channel_hint')}
				</p>
				<div class="mt-2.5 flex items-center gap-2">
					<Input
						bind:value={channelInput}
						placeholder={t('settings.twitch.channel_placeholder')}
						spellcheck={false}
						autocomplete="off"
					/>
					<Button
						variant="outline"
						size="sm"
						disabled={busy || channelInput.trim() === (twitch.config.channelLogin ?? '')}
						onclick={saveChannel}
					>
						{t('common.save')}
					</Button>
					{#if twitch.config.channelLogin}
						<Button variant="ghost" size="sm" disabled={busy} onclick={clearChannel}>
							{t('settings.twitch.channel_clear')}
						</Button>
					{/if}
				</div>
			</div>

			{#if twitch.channel && !twitch.channelPointsAvailable}
				<!-- Not an error and not retryable: it is a property of the channel, and the earlier
				     it is said the fewer rewards get configured that could never be created. -->
				<div class="px-4 py-3">
					<Alert>
						<AlertDescription>
							{t('settings.twitch.channel_points_unavailable')}
						</AlertDescription>
					</Alert>
				</div>
			{/if}
		</div>
	</section>
{/if}

{#if chatReady}
	<section class={GROUP}>
		<h3 class={LABEL}>{t('settings.twitch.chat')}</h3>
		<div class={CARD}>
			<div class={ROW}>
				<div class="min-w-0">
					<p class="flex items-center gap-2 text-sm font-medium">
						<!-- One dot, three states: this is the only thing on the panel that changes
						     without the user doing anything, so it carries the liveness signal. -->
						<span
							class="inline-block size-2 shrink-0 rounded-full {chat.status === 'live'
								? 'bg-primary'
								: chat.status === 'failed'
									? 'bg-destructive'
									: 'bg-muted-foreground'}"
						></span>
						{chatLabel}
					</p>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{#if chat.status === 'live' && (chat.messages > 0 || chat.duplicates > 0)}
							{t('settings.twitch.chat_counts', {
								messages: chat.messages,
								duplicates: chat.duplicates
							})}
						{:else if chat.status === 'live'}
							{t('settings.twitch.chat_counts_zero')}
						{:else}
							{t('settings.twitch.chat_hint')}
						{/if}
					</p>
					{#if sessionShort}
						<p class="mt-1 font-mono text-[11px] text-muted-foreground">
							{t('settings.twitch.chat_session', { id: sessionShort })}
						</p>
					{/if}
				</div>
			</div>

			{#if chat.error}
				<div class="px-4 py-3">
					<Alert variant="destructive">
						<AlertDescription>{chat.error}</AlertDescription>
					</Alert>
				</div>
			{/if}

			{#if chat.recent.length}
				<ul class="max-h-72 overflow-y-auto">
					{#each chat.recent as m (m.messageId)}
						<li class="flex items-baseline gap-2 px-4 py-1.5 text-xs">
							<span class="flex shrink-0 items-baseline gap-1">
								{#each roles(m) as r (r)}
									<span
										class="rounded border px-1 text-[9px] font-semibold text-muted-foreground"
										>{r}</span
									>
								{/each}
								<span class="font-medium">{m.chatterUserName}</span>
							</span>
							<span class="min-w-0 text-muted-foreground">{m.text}</span>
							{#if m.bits}
								<span class="ml-auto shrink-0 text-[10px] text-primary">{m.bits} bits</span>
							{/if}
						</li>
					{/each}
				</ul>
			{:else if chat.status === 'live'}
				<p class="px-4 py-3 text-xs text-muted-foreground">
					{t('settings.twitch.chat_waiting')}
				</p>
			{/if}
		</div>
	</section>
{/if}


{#if connected && twitch.channel}
	<section class={GROUP}>
		<h3 class={LABEL}>{t('settings.twitch.requests')}</h3>
		<div class={CARD}>
			<div class={ROW}>
				<div class="min-w-0">
					<p class="text-sm font-medium">{t('settings.twitch.requests_enabled')}</p>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{t('settings.twitch.requests_enabled_hint')}
					</p>
				</div>
				<Switch bind:checked={requestsEnabled} />
			</div>

			<div class={ROW}>
				<div class="min-w-0">
					<p class="text-sm font-medium">{t('settings.twitch.requests_command')}</p>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{t('settings.twitch.requests_command_hint')}
					</p>
				</div>
				<Input class="w-52" bind:value={commandInput} placeholder="!sr !songrequest" />
			</div>

			<div class={ROW}>
				<div class="min-w-0">
					<p class="text-sm font-medium">{t('settings.twitch.requests_role')}</p>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{t('settings.twitch.requests_role_hint')}
					</p>
				</div>
				<select
					class="h-9 rounded-md border bg-transparent px-3 text-sm"
					bind:value={minRole}
				>
					{#each ROLES as role (role)}
						<option value={role}>{t(`settings.twitch.role_${role}`)}</option>
					{/each}
				</select>
			</div>

			<div class={ROW}>
				<div class="min-w-0">
					<p class="text-sm font-medium">{t('settings.twitch.requests_cooldown')}</p>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{t('settings.twitch.requests_cooldown_hint')}
					</p>
				</div>
				<div class="flex items-center gap-3">
					<label class="flex items-center gap-2 text-xs text-muted-foreground">
						{t('settings.twitch.requests_cooldown_user')}
						<Input class="w-20" type="number" min="0" max="3600" bind:value={userCooldown} />
					</label>
					<label class="flex items-center gap-2 text-xs text-muted-foreground">
						{t('settings.twitch.requests_cooldown_global')}
						<Input class="w-20" type="number" min="0" max="3600" bind:value={globalCooldown} />
					</label>
				</div>
			</div>

			<div class={ROW}>
				<div class="min-w-0">
					<p class="text-sm font-medium">{t('settings.twitch.requests_reply')}</p>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{t('settings.twitch.requests_reply_hint')}
					</p>
				</div>
				<Switch bind:checked={replyInChat} />
			</div>

			<div class={ROW}>
				<div class="min-w-0">
					<p class="text-sm font-medium">{t('settings.twitch.requests_reward')}</p>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{t('settings.twitch.requests_reward_hint')}
					</p>
				</div>
				<div class="flex items-center gap-2">
					<select
						class="h-9 max-w-56 rounded-md border bg-transparent px-3 text-sm"
						bind:value={rewardId}
						disabled={rewardBusy}
					>
						<option value="">{t('settings.twitch.requests_reward_none')}</option>
						{#each rewards as reward (reward.id)}
							<option value={reward.id}>
								{reward.enabled ? reward.title : t('settings.twitch.requests_reward_paused', { title: reward.title })}
							</option>
						{/each}
					</select>
					<Button variant="outline" size="sm" disabled={rewardBusy} onclick={loadRewards}>
						<HugeiconsIcon icon={RefreshIcon} size={14} />
					</Button>
				</div>
			</div>

			{#if rewardsError}
				<p class="px-4 pb-3 text-xs text-destructive">{rewardsError}</p>
			{/if}
			{#if requestsError}
				<p class="px-4 pb-3 text-xs text-destructive">{requestsError}</p>
			{/if}

			<div class={ROW}>
				<p class="text-xs text-muted-foreground">{t('settings.twitch.requests_footer')}</p>
				<Button size="sm" disabled={busy} onclick={saveRequests}>
					{t('settings.twitch.requests_save')}
				</Button>
			</div>
		</div>
	</section>
{/if}

<section class={GROUP}>
	<h3 class={LABEL}>{t('settings.twitch.scopes')}</h3>
	<div class={CARD}>
		<div class="px-4 py-3">
			<p class="text-sm font-medium">
				{t('settings.twitch.scope_count', { count: twitch.scopes.length })}
			</p>
			<p class="mt-0.5 text-xs text-muted-foreground">{t('settings.twitch.scopes_hint')}</p>
			{#if twitch.scopes.length}
				<ul class="mt-2 flex flex-wrap gap-1.5">
					{#each twitch.scopes as scope (scope)}
						<li class="rounded-md border bg-muted/50 px-2 py-0.5 font-mono text-[11px]">
							{scope}
						</li>
					{/each}
				</ul>
			{/if}
			{#if connected}
				<p class="mt-2 flex items-center gap-1.5 text-xs text-muted-foreground">
					<HugeiconsIcon icon={Tick02Icon} class="h-3.5 w-3.5 text-primary" />
					{t('settings.twitch.token_expires', { when: clock(twitch.expiresAt) })}
				</p>
				<p class="mt-1 flex items-center gap-1.5 text-xs text-muted-foreground">
					<HugeiconsIcon icon={RefreshIcon} class="h-3.5 w-3.5" />
					{t('settings.twitch.validated', { when: clock(twitch.validatedAt) })}
				</p>
			{/if}
		</div>
	</div>
</section>
