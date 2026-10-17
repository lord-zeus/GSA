import { defineStore } from 'pinia'
import { api, type CatalogEngine, type ImportResult } from '@/api/client'

export const useCatalogStore = defineStore('catalog', {
  state: () => ({
    items: [] as CatalogEngine[],
    total: 0,
    importing: false,
    importResult: null as ImportResult | null,
    error: null as string | null,
  }),
  actions: {
    async load(q?: { kind?: string; search?: string; offset?: number; limit?: number }) {
      this.error = null
      const res = await api.catalogList(q)
      this.items = res.data
      this.total = res.total
      return res
    },
    async reimportAll() {
      this.importing = true
      this.error = null
      try {
        this.importResult = await api.catalogImport()
        await this.load()
      } catch (e: any) {
        this.error = e?.message ?? String(e)
      } finally {
        this.importing = false
      }
    },
  },
})
