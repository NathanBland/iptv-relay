import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

export function formatBitrate(kbps: number) {
  return kbps >= 1_000 ? `${(kbps / 1_000).toFixed(1)} Mbps` : `${kbps} Kbps`
}

export function formatRelativeTime(iso: string, now = new Date()) {
  const minutes = Math.round((now.getTime() - new Date(iso).getTime()) / 60_000)
  if (minutes < 1) return 'just now'
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.round(minutes / 60)
  return hours < 24 ? `${hours}h ago` : `${Math.round(hours / 24)}d ago`
}
