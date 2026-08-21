import { Check, ChevronDown, Search } from 'lucide-react'
import { useEffect, useId, useMemo, useRef, useState } from 'react'
import { cn } from '@/lib/utils'

export interface ComboboxItem {
  value: string
  label: string
  hint?: string
}

interface ComboboxProps {
  items: ComboboxItem[]
  value: string
  onChange: (value: string) => void
  placeholder?: string
  searchPlaceholder?: string
  emptyText?: string
  className?: string
  ariaLabel?: string
}

/**
 * Searchable dropdown combobox. Renders a trigger button that opens
 * a popover with a filter input and a scrollable list of options.
 */
export function Combobox({
  items,
  value,
  onChange,
  placeholder = 'Select…',
  searchPlaceholder = 'Search…',
  emptyText = 'No items found.',
  className,
  ariaLabel,
}: ComboboxProps) {
  const [open, setOpen] = useState(false)
  const [query, setQuery] = useState('')
  const containerRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const listId = useId()

  const selected = useMemo(
    () => items.find((item) => item.value === value),
    [items, value],
  )

  const filtered = useMemo(() => {
    const lower = query.toLowerCase().trim()
    if (!lower) return items
    return items.filter(
      (item) =>
        item.label.toLowerCase().includes(lower) ||
        item.value.toLowerCase().includes(lower),
    )
  }, [items, query])

  useEffect(() => {
    if (!open) return
    function handleClickOutside(event: MouseEvent) {
      if (containerRef.current && !containerRef.current.contains(event.target as Node)) {
        setOpen(false)
        setQuery('')
      }
    }
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        setOpen(false)
        setQuery('')
      }
    }
    document.addEventListener('mousedown', handleClickOutside)
    document.addEventListener('keydown', handleKeyDown)
    return () => {
      document.removeEventListener('mousedown', handleClickOutside)
      document.removeEventListener('keydown', handleKeyDown)
    }
  }, [open])

  useEffect(() => {
    if (open) {
      requestAnimationFrame(() => inputRef.current?.focus())
    } else {
      setQuery('')
    }
  }, [open])

  function select(itemValue: string) {
    onChange(itemValue)
    setOpen(false)
    setQuery('')
  }

  return (
    <div ref={containerRef} className={cn('relative', className)}>
      <button
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={ariaLabel}
        className="flex h-10 w-full items-center justify-between gap-2 rounded-lg border border-white/12 bg-ink-950/70 px-3 text-sm text-white transition-colors hover:bg-white/5"
        onClick={() => setOpen((prev) => !prev)}
      >
        <span className={cn('truncate', !selected && 'text-slate-500')}>
          {selected ? selected.label : placeholder}
        </span>
        <ChevronDown
          aria-hidden="true"
          className={cn('size-4 shrink-0 text-slate-500 transition-transform', open && 'rotate-180')}
        />
      </button>

      {open ? (
        <div className="absolute left-0 right-0 top-full z-50 mt-1 overflow-hidden rounded-lg border border-white/12 bg-ink-900 shadow-xl shadow-black/40">
          <div className="relative border-b border-white/8 p-2">
            <Search
              aria-hidden="true"
              className="pointer-events-none absolute left-4 top-1/2 size-4 -translate-y-1/2 text-slate-500"
            />
            <input
              ref={inputRef}
              type="text"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={searchPlaceholder}
              className="h-8 w-full rounded-md bg-white/5 pl-8 pr-3 text-sm text-white placeholder:text-slate-500 focus:outline-none focus:ring-1 focus:ring-ocean-400"
              role="combobox"
              aria-expanded={open}
              aria-controls={listId}
            />
          </div>
          <ul
            id={listId}
            role="listbox"
            className="max-h-60 overflow-y-auto py-1"
          >
            {filtered.length === 0 ? (
              <li className="px-3 py-6 text-center text-xs text-slate-500">{emptyText}</li>
            ) : (
              filtered.map((item) => {
                const isSelected = item.value === value
                return (
                  <li key={item.value} role="option" aria-selected={isSelected}>
                    <button
                      type="button"
                      className={cn(
                        'flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-sm transition-colors hover:bg-white/5',
                        isSelected ? 'text-ocean-400' : 'text-slate-200',
                      )}
                      onClick={() => select(item.value)}
                    >
                      <span className="flex flex-col">
                        <span className="truncate">{item.label}</span>
                        {item.hint ? (
                          <span className="text-[0.68rem] text-slate-500">{item.hint}</span>
                        ) : null}
                      </span>
                      {isSelected ? (
                        <Check aria-hidden="true" className="size-4 shrink-0 text-ocean-400" />
                      ) : null}
                    </button>
                  </li>
                )
              })
            )}
          </ul>
        </div>
      ) : null}
    </div>
  )
}
