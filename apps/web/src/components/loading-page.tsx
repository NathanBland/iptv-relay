export function LoadingPage({ label }: { label: string }) {
  return (
    <div role="status" className="grid min-h-72 place-items-center rounded-xl border border-white/10 bg-ink-900/70 p-8 text-center">
      <div>
        <span aria-hidden="true" className="mx-auto block size-8 animate-spin rounded-full border-2 border-white/10 border-t-ocean-400" />
        <p className="mt-4 text-sm font-medium text-slate-300">Wait while the system loads {label}…</p>
      </div>
    </div>
  )
}
