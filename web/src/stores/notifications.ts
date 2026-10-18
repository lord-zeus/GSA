import { defineStore } from 'pinia'
import { api, NotificationItem, NotifySettings } from '@/api/client'

export interface ToastItem {
  id: string
  type: string
  severity: string
  title: string
  message: string
  at: number
}

export interface NotificationStore {
  items: NotificationItem[]
  unread: number
  live: boolean
  toasts: ToastItem[]
  settings: NotifySettings | null
  load: () => Promise<void>
  connect: () => void
  disconnect: () => void
  markRead: (id: string) => Promise<void>
  markAllRead: () => Promise<void>
  dismissToast: (id: string) => void
  loadSettings: () => Promise<void>
}

let seq = 0
const nextId = () => `t${Date.now()}-${(seq++).toString(36)}`

let es: EventSource | null = null

export const useNotificationStore = defineStore('notifications', {
  state: (): {
    items: NotificationItem[]
    unread: number
    live: boolean
    toasts: ToastItem[]
    settings: NotifySettings | null
  } => ({
    items: [],
    unread: 0,
    live: false,
    toasts: [],
    settings: null,
  }),
  getters: {
    lastUnread: (s) => {
      const u = s.items.filter((i) => !i.is_read)
      return u.length ? u[0] : null
    },
  },
  actions: {
    async load() {
      try {
        const [list, count] = await Promise.all([
          api.notificationsList({ limit: 200 }),
          api.notificationsUnread(),
        ])
        this.items = list
        this.unread = count.unread
      } catch (e: any) {
        // bell stays silent when the API is down; the health dot tells that story
        console.warn('notifications load failed:', e?.message ?? e)
      }
    },

    connect() {
      if (es) return
      this.live = true

      es = new EventSource('/api/notifications/live')
      const stream = es

      stream.addEventListener('hello', (e) => {
        // (re)connected: the channel is live again, and we refetch the DB
        // list — it is the source of truth, so any alert missed while the
        // page was closed is picked up here.
        this.live = true
        if (e.data) this.load()
      })

      stream.addEventListener('notify', (e) => {
        if (!e.data) return
        let n: NotificationItem
        try {
          n = JSON.parse(e.data)
        } catch {
          return
        }
        if (!this.items.some((i) => i.id === n.id)) {
          this.items.unshift(n)
          if (this.items.length > 500) this.items.length = 500
        }
        this.unread += 1
        this.pushToast(n)
      })

      stream.onerror = () => {
        // EventSource auto-reconnects; mark the channel visually stale so the
        // UI can show "reconnecting…" until the next `hello` lands.
        this.live = false
      }

      this.load()
    },

    disconnect() {
      es?.close()
      es = null
      this.live = false
    },

    pushToast(n: NotificationItem) {
      const id = nextId()
      this.toasts.unshift({
        id,
        type: n.type,
        severity: n.severity,
        title: n.title,
        message: n.message,
        at: Date.now(),
      })
      if (this.toasts.length > 6) this.toasts.length = 6
      setTimeout(() => this.dismissToast(id), 12000)
    },

    dismissToast(id: string) {
      const i = this.toasts.findIndex((t) => t.id === id)
      if (i >= 0) this.toasts.splice(i, 1)
    },

    async markRead(id: string) {
      await api.notificationMarkRead(id)
      const n = this.items.find((i) => i.id === id)
      if (n && !n.is_read) {
        n.is_read = true
        this.unread = Math.max(0, this.unread - 1)
      }
    },

    async markAllRead() {
      await api.notificationsReadAll()
      for (const n of this.items) if (!n.is_read) n.is_read = true
      this.unread = 0
    },

    async loadSettings() {
      this.settings = await api.notifySettingsGet()
    },
  },
})
