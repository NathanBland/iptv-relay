import { useEffect, useRef, useState } from 'react'
import mpegts from 'mpegts.js'
import { AlertCircle, Loader2, Play, Square, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card } from '@/components/ui/card'
import type { IptvApiClient } from '@/lib/api/types'

interface StreamPreviewProps {
  channelId: string
  channelName: string
  client: IptvApiClient
  onClose: () => void
}

export function StreamPreview({ channelId, channelName, client, onClose }: StreamPreviewProps) {
  const videoRef = useRef<HTMLVideoElement>(null)
  const playerRef = useRef<ReturnType<typeof mpegts.createPlayer> | null>(null)
  const [status, setStatus] = useState<'loading' | 'playing' | 'error' | 'stopped'>('loading')
  const [error, setError] = useState('')

  useEffect(() => {
    let cancelled = false

    async function startPlayback() {
      try {
        const preview = await client.getChannelPreview(channelId)
        if (cancelled) return

        if (!mpegts.getFeatureList().mseLivePlayback) {
          setStatus('error')
          setError('Your browser does not support MPEG-TS live playback via Media Source Extensions.')
          return
        }

        const video = videoRef.current
        if (!video) return

        const player = mpegts.createPlayer({
          type: 'mpegts',
          isLive: true,
          url: preview.streamUrl,
          cors: true,
        }, {
          liveBufferLatencyChasing: true,
          liveBufferLatencyMaxLatency: 1.5,
          liveBufferLatencyMinRemain: 0.3,
        })

        player.attachMediaElement(video)
        player.load()
        player.play()
        playerRef.current = player
        setStatus('playing')
      } catch (err) {
        if (cancelled) return
        setStatus('error')
        setError(err instanceof Error ? err.message : 'Failed to load stream preview.')
      }
    }

    startPlayback()

    return () => {
      cancelled = true
      const player = playerRef.current
      if (player) {
        try {
          player.pause()
          player.unload()
          player.detachMediaElement()
          player.destroy()
        } catch { /* player already destroyed */ }
        playerRef.current = null
      }
    }
  }, [channelId, client])

  function handleStop() {
    const player = playerRef.current
    if (player) {
      try {
        player.pause()
        player.unload()
      } catch { /* ignore */ }
    }
    setStatus('stopped')
  }

  function handleResume() {
    const player = playerRef.current
    const video = videoRef.current
    if (player && video) {
      try {
        player.load()
        player.play()
        setStatus('playing')
      } catch {
        setStatus('error')
        setError('Failed to resume playback.')
      }
    }
  }

  return (
    <Card className="mb-4 overflow-hidden">
      <div className="flex items-center justify-between border-b border-white/8 p-3">
        <div className="flex items-center gap-2">
          <p className="text-sm font-semibold text-white">Preview: {channelName}</p>
          {status === 'loading' && <Loader2 aria-hidden="true" className="size-4 animate-spin text-ocean-400" />}
          {status === 'playing' && <span className="text-xs text-emerald-400">Live</span>}
          {status === 'error' && <AlertCircle aria-hidden="true" className="size-4 text-rose-400" />}
          {status === 'stopped' && <span className="text-xs text-slate-500">Stopped</span>}
        </div>
        <div className="flex gap-2">
          {status === 'playing' && (
            <Button variant="ghost" size="sm" onClick={handleStop}>
              <Square aria-hidden="true" className="size-4" /> Stop
            </Button>
          )}
          {status === 'stopped' && (
            <Button variant="ghost" size="sm" onClick={handleResume}>
              <Play aria-hidden="true" className="size-4" /> Resume
            </Button>
          )}
          <Button variant="ghost" size="sm" onClick={onClose}>
            <X aria-hidden="true" className="size-4" /> Close
          </Button>
        </div>
      </div>
      <div className="relative bg-black">
        <video ref={videoRef} className="mx-auto max-h-64 w-full" controls muted playsInline />
        {status === 'error' && (
          <div role="alert" className="p-4 text-sm text-rose-300">
            {error}
          </div>
        )}
        {status === 'loading' && (
          <div className="flex items-center justify-center p-8 text-sm text-slate-500">
            <Loader2 aria-hidden="true" className="mr-2 size-4 animate-spin" /> Connecting to stream…
          </div>
        )}
      </div>
    </Card>
  )
}
