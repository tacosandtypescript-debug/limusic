<script lang="ts">
	// Settings ▸ Overlay: the OBS browser-source link, and the controls to shape what it renders.
	//
	// The preview is the real thing in an iframe — not a mock. That is the whole reason this panel
	// has this shape: six overlays are impossible to choose between from a description, and a
	// hand-drawn thumbnail would drift from the page the moment either changed. The iframe loads the
	// same URL OBS will.
	//
	// There is no orientation switch any more. Each overlay declares its own base size, so the
	// preview's aspect ratio comes from the selection — which is the honest thing to show, and it is
	// the number the streamer has to type into the browser source anyway.
	import { HugeiconsIcon } from '@hugeicons/svelte';
	import { Copy01Icon, Tick02Icon } from '@hugeicons/core-free-icons';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Slider } from '$lib/components/ui/slider';
	import { Alert, AlertDescription } from '$lib/components/ui/alert';
	import * as api from '$lib/api';
	import { copyText } from '$lib/clipboard';
	import { toast } from '$lib/player.svelte';
	import { t } from '$lib/i18n.svelte';

	const GROUP = 'mb-7 last:mb-1';
	const LABEL =
		'mb-2 px-1 text-[11px] font-semibold uppercase tracking-[0.08em] text-muted-foreground';
	const CARD = 'divide-y divide-border/60 overflow-hidden rounded-xl border bg-card';

	/** One row per overlay. `w`/`h` are the design's base box — the source of truth lives in
	 *  `overlay/page.html`, and these have to match it, because the browser source is typed in by
	 *  hand from what this panel shows. */
	const CATALOGUE = [
		{ id: 'sleeve-wide', name: 'design_sleeve', variant: 'variant_horizontal', w: 620, h: 200 },
		{ id: 'sleeve-tall', name: 'design_sleeve', variant: 'variant_vertical', w: 380, h: 430 },
		{ id: 'playout-wide', name: 'design_playout', variant: 'variant_horizontal', w: 760, h: 222 },
		{ id: 'playout-tall', name: 'design_playout', variant: 'variant_vertical', w: 380, h: 430 },
		{ id: 'vinyl-wide', name: 'design_vinyl', variant: 'variant_horizontal', w: 680, h: 230 },
		{ id: 'vinyl-tall', name: 'design_vinyl', variant: 'variant_vertical', w: 400, h: 470 }
	] as const;

	let info = $state<api.OverlayInfo | null>(null);
	api.overlayInfo()
		.then((v) => (info = v))
		.catch(() => {});

	/** How the overlay sits on the scene. `auto` is the design's own surface — the appearance that
	 *  was measured against hostile backgrounds — and the rest are choices laid on top of it. */
	const CARDS = ['auto', 'transparent', 'solid', 'translucent', 'glass'] as const;
	/** Bundles, so nobody has to set eight options by hand to get a coherent overlay. `default`
	 *  means "no preset": the design's own look. */
	const PRESETS = ['default', 'minimal', 'card', 'glass', 'dynamic', 'vinyl'] as const;

	/** Which surface each preset chooses, mirroring `PRESETS` in `overlay/page.html`.
	 *
	 *  A preset is a *bundle*, and the background is one of the things in it — not a separate
	 *  control that happens to sit next to it. That is why both rows used to look like two ways of
	 *  saying the same thing: "Glass" appeared in each, and the preset called "Card" produced a
	 *  background called "Solid", with nothing on screen connecting them.
	 *
	 *  So picking a preset moves the background row to whatever the preset chose. It shows the
	 *  relationship instead of describing it, and it makes the override obvious: choose a background
	 *  afterwards and it stays where you put it, because an explicit `card` beats the preset's. */
	const PRESET_CARD: Record<string, string> = {
		minimal: 'transparent', card: 'solid', glass: 'glass', dynamic: 'translucent', vinyl: 'auto'
	};

	function choosePreset(p: string) {
		preset = p;
		const chosen = PRESET_CARD[p];
		if (chosen) card = chosen;
	}

	const store = (k: string, d: string) => localStorage.getItem(k) ?? d;
	let design = $state(store('overlay_design', 'sleeve-wide'));
	let pos = $state(store('overlay_pos', 'bl'));
	let scale = $state(Number(store('overlay_scale', '1')) || 1);
	let card = $state(store('overlay_card', 'auto'));
	let preset = $state(store('overlay_preset', 'default'));
	// Preview-only. Never reaches the copied URL: `demo` would freeze the overlay on sample data
	// during a stream, and `idle=show` would leave a blank card on screen.
	let demo = $state(true);

	$effect(() => {
		localStorage.setItem('overlay_design', design);
		localStorage.setItem('overlay_pos', pos);
		localStorage.setItem('overlay_scale', String(scale));
		localStorage.setItem('overlay_card', card);
		localStorage.setItem('overlay_preset', preset);
	});

	/** The options that shape the overlay, in the order they read best in a URL. Only what differs
	 *  from the defaults is written, so a copied link stays short enough to read. */
	const look = $derived(
		(card === 'auto' ? '' : `&card=${card}`) + (preset === 'default' ? '' : `&preset=${preset}`)
	);

	const chosen = $derived(CATALOGUE.find((o) => o.id === design) ?? CATALOGUE[0]);

	const ANCHORS: { id: string; label: string }[] = [
		{ id: 'tl', label: '↖' }, { id: 'tc', label: '↑' }, { id: 'tr', label: '↗' },
		{ id: 'ml', label: '←' }, { id: 'mc', label: '·' }, { id: 'mr', label: '→' },
		{ id: 'bl', label: '↙' }, { id: 'bc', label: '↓' }, { id: 'br', label: '↘' }
	];

	const available = $derived(!!info?.available);
	const previewSrc = $derived(
		available
			? `${info!.baseUrl}?design=${design}&pos=${pos}&scale=${scale}&idle=show${look}${demo ? '&demo=1' : ''}`
			: ''
	);
	/** What goes into OBS: the chosen overlay, and nothing that only makes sense in a preview. */
	const obsUrl = $derived(
		available ? `${info!.baseUrl}?design=${design}&pos=${pos}&scale=${scale}${look}` : ''
	);

	let copied = $state(false);
	async function copy() {
		try {
			await copyText(obsUrl);
			copied = true;
			setTimeout(() => (copied = false), 1600);
		} catch (e) {
			toast.error(String(e));
		}
	}
</script>

<section class={GROUP}>
	<h3 class={LABEL}>{t('settings.overlay.status')}</h3>
	<div class={CARD}>
		<div class="px-4 py-3">
			<p class="flex items-center gap-2 text-sm font-medium">
				<span
					class="inline-block size-2 shrink-0 rounded-full {available ? 'bg-primary' : 'bg-destructive'}"
				></span>
				{available ? t('settings.overlay.status_up') : t('settings.overlay.status_down')}
			</p>
			<p class="mt-0.5 text-xs text-muted-foreground">{t('settings.overlay.status_hint')}</p>
			{#if available && info?.portFellBack}
				<!-- Worth saying out loud: the link still works, but it is not the port they configured,
				     and a stale OBS source pointing at the old one would just go black. -->
				<div class="mt-2.5">
					<Alert>
						<AlertDescription>
							{t('settings.overlay.port_fell_back', { port: info.port })}
						</AlertDescription>
					</Alert>
				</div>
			{/if}
		</div>
	</div>
</section>

{#if available}
	<section class={GROUP}>
		<h3 class={LABEL}>{t('settings.overlay.design')}</h3>
		<div class={CARD}>
			<div class="px-4 py-3">
				<!-- The preview: the real page, at the real URL, in the selected overlay's own aspect
				     ratio. The frame is sized from `w`/`h`, so what is on screen is the shape the
				     browser source will have. -->
				<div
					class="mx-auto overflow-hidden rounded-lg border bg-[oklch(0.145_0.008_285)]"
					style="aspect-ratio: {chosen.w} / {chosen.h}; width: min(100%, {Math.round(
						(chosen.w / chosen.h) * 240
					)}px);"
				>
					{#if previewSrc}
						<iframe
							src={previewSrc}
							title={t('settings.overlay.design')}
							class="h-full w-full border-0"
						></iframe>
					{/if}
				</div>

				<p class="mt-2 text-center text-xs text-muted-foreground">
					{t('settings.overlay.base_size', { w: chosen.w, h: chosen.h })}
				</p>
				<div class="mt-2 flex justify-center">
					<Button variant={demo ? 'default' : 'outline'} size="sm" onclick={() => (demo = !demo)}>
						{t('settings.overlay.sample')}
					</Button>
				</div>
				<p class="mt-2 text-center text-xs text-muted-foreground">
					{demo ? t('settings.overlay.sample_hint') : t('settings.overlay.live_hint')}
				</p>
			</div>

			<!-- Background and preset sit directly under the preview rather than three sections
			     further down: they are the controls you change *while watching* the preview, so
			     having to scroll past the design list and the placement settings to reach them, then
			     scroll back up to see what changed, was the wrong order for the way they are used. -->
			<div class="px-4 py-3">
				<p class="text-sm font-medium">{t('settings.overlay.card')}</p>
				<p class="mt-0.5 text-xs text-muted-foreground">{t('settings.overlay.card_hint')}</p>
				<div class="mt-2.5 flex flex-wrap gap-1.5">
					{#each CARDS as c (c)}
						<button
							type="button"
							aria-pressed={card === c}
							onclick={() => (card = c)}
							class="cursor-pointer rounded-full border px-3 py-1 text-xs transition-colors {card === c
								? 'border-primary/60 bg-primary/15 text-foreground'
								: 'border-border text-muted-foreground hover:bg-foreground/5'}"
						>
							{t(`settings.overlay.card_${c}`)}
						</button>
					{/each}
				</div>
			</div>
			<div class="px-4 py-3">
				<p class="text-sm font-medium">{t('settings.overlay.preset')}</p>
				<p class="mt-0.5 text-xs text-muted-foreground">{t('settings.overlay.preset_hint')}</p>
				<div class="mt-2.5 flex flex-wrap gap-1.5">
					{#each PRESETS as p (p)}
						<button
							type="button"
							aria-pressed={preset === p}
							onclick={() => choosePreset(p)}
							class="cursor-pointer rounded-full border px-3 py-1 text-xs transition-colors {preset ===
							p
								? 'border-primary/60 bg-primary/15 text-foreground'
								: 'border-border text-muted-foreground hover:bg-foreground/5'}"
						>
							{t(`settings.overlay.preset_${p}`)}
						</button>
					{/each}
				</div>
			</div>

			<div class="px-4 py-3">
				<div class="grid grid-cols-1 gap-1.5 sm:grid-cols-2">
					{#each CATALOGUE as o (o.id)}
						<button
							type="button"
							aria-pressed={design === o.id}
							onclick={() => (design = o.id)}
							class="cursor-pointer rounded-lg border px-3 py-2 text-left transition-colors {design ===
							o.id
								? 'border-primary/60 bg-primary/10'
								: 'border-border hover:bg-foreground/5'}"
						>
							<span class="flex items-center gap-2 text-sm font-medium">
								{#if design === o.id}
									<HugeiconsIcon icon={Tick02Icon} class="h-3.5 w-3.5 shrink-0 text-primary" />
								{/if}
								{t(`settings.overlay.${o.name}`)}
								<span class="text-muted-foreground">· {t(`settings.overlay.${o.variant}`)}</span>
							</span>
							<span class="mt-0.5 block font-mono text-[11px] text-muted-foreground">
								{o.w}×{o.h}
							</span>
						</button>
					{/each}
				</div>
			</div>
		</div>
	</section>

	<section class={GROUP}>
		<h3 class={LABEL}>{t('settings.overlay.placement')}</h3>
		<div class={CARD}>
			<div class="flex items-center justify-between gap-4 px-4 py-3">
				<div class="min-w-0">
					<p class="text-sm font-medium">{t('settings.overlay.position')}</p>
					<p class="mt-0.5 text-xs text-muted-foreground">
						{t('settings.overlay.position_hint')}
					</p>
				</div>
				<div class="grid shrink-0 grid-cols-3 gap-1">
					{#each ANCHORS as a (a.id)}
						<button
							type="button"
							aria-label={a.id}
							aria-pressed={pos === a.id}
							onclick={() => (pos = a.id)}
							class="grid size-6 cursor-pointer place-items-center rounded border text-[11px] leading-none transition-colors {pos ===
							a.id
								? 'border-primary/60 bg-primary/15 text-foreground'
								: 'border-border text-muted-foreground hover:bg-foreground/5'}"
						>
							{a.label}
						</button>
					{/each}
				</div>
			</div>
			<div class="flex items-center justify-between gap-4 px-4 py-3">
				<div class="min-w-0">
					<p class="text-sm font-medium">{t('settings.overlay.scale')}</p>
					<p class="mt-0.5 text-xs text-muted-foreground">{t('settings.overlay.scale_hint')}</p>
				</div>
				<div class="flex shrink-0 items-center gap-3">
					<Slider
						type="single"
						aria-label={t('settings.overlay.scale')}
						min={0.5}
						max={3}
						step={0.05}
						value={scale}
						onValueChange={(v) => (scale = v)}
						class="w-36"
					/>
					<span class="w-9 shrink-0 text-right text-xs tabular-nums text-muted-foreground">
						{scale.toFixed(2)}×
					</span>
				</div>
			</div>
		</div>
	</section>

	<section class={GROUP}>
		<h3 class={LABEL}>{t('settings.overlay.link')}</h3>
		<div class={CARD}>
			<div class="px-4 py-3">
				<p class="text-sm font-medium">{t('settings.overlay.link_title')}</p>
				<p class="mt-0.5 text-xs text-muted-foreground">{t('settings.overlay.link_hint')}</p>
				<div class="mt-2.5 flex items-center gap-2">
					<!-- Read-only rather than disabled: the point is to be able to read and select it,
					     and the Copy button is the primary way out anyway. -->
					<Input value={obsUrl} readonly class="font-mono text-[11px]" />
					<Button variant="outline" size="sm" onclick={copy}>
						<HugeiconsIcon icon={copied ? Tick02Icon : Copy01Icon} class="mr-1.5 h-3.5 w-3.5" />
						{copied ? t('settings.overlay.copied') : t('common.copy')}
					</Button>
				</div>
			</div>
			<div class="px-4 py-3">
				<p class="text-sm font-medium">{t('settings.overlay.obs')}</p>
				<ol class="mt-1.5 flex list-decimal flex-col gap-1 pl-5 text-xs text-muted-foreground">
					<li>{t('settings.overlay.obs_1')}</li>
					<li>{t('settings.overlay.obs_2', { w: chosen.w, h: chosen.h })}</li>
					<li>{t('settings.overlay.obs_3')}</li>
					<li>{t('settings.overlay.obs_4')}</li>
				</ol>
			</div>
		</div>
	</section>
{/if}
