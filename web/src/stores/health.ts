import { defineStore } from 'pinia'
import { api } from '@/api/client'

export interface HealthState {
  status: 'ok' | 'degraded' | 'down'
  db: 'up' | 'down' | 'unknown'
  checkedAt: number | null
  error: string | null
}

export const useHealthStore = defineStore('health', {
  state: (): HealthState => ({
    status: 'down',
    db: 'unknown',
    checkedAt: null,
    error: null,
  }),
  actions: {
    async refresh() {
      try {
        const h = await api.health()
        this.status = h.status === 'ok' ? 'ok' : 'degraded'
        this.db = h.db === 'up' ? 'up' : 'down'
        this.checkedAt = Date.now()
        this.error = null
      } catch (e: any) {
        this.status = 'down'
        this.db = 'unknown'
        this.checkedAt = Date.now()
        this.error = e?.message ?? 'unreachable'
      }
    },
  },
})
