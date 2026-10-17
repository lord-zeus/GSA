<script setup lang="ts">
import { ref, onMounted, computed } from 'vue'
import { useCatalogStore } from '@/stores/catalog'

const store = useCatalogStore()
const search = ref('')
const kind = ref('')
const offset = ref(0)
const PER_PAGE = 25

const canPrev = computed(() => offset.value > 0)
const canNext = computed(() => offset.value + PER_PAGE < store.total)

async function reload() {
  offset.value = 0
  await store.load({ search: search.value || undefined, kind: kind.value || undefined, offset: 0, limit: PER_PAGE })
}
function next() { offset.value += PER_PAGE; store.load({ search: search.value || undefined, kind: kind.value || undefined, offset: offset.value, limit: PER_PAGE }) }
function prev() { offset.value = Math.max(0, offset.value - PER_PAGE); store.load({ search: search.value || undefined, kind: kind.value || undefined, offset: offset.value, limit: PER_PAGE }) }

onMounted(() => store.load({ limit: PER_PAGE, offset: 0 }).catch(() => {}))
</script>

<template>
  <div>
    <div class="page-head">
      <div>
        <h1>Engine registry</h1>
        <p>
          {{ store.total.toLocaleString() }} real GSA SER engines (web + email) imported from
          <span class="mono">~2.3 MB of .ini specs</span> — search, filter, inspect.
        </p>
      </div>
    </div>

    <div class="row" style="margin-bottom:14px">
      <input
        v-model="search"
        placeholder="Search by name (e.g. 'aardvark')…"
        style="flex:1;min-width:220px"
        @keydown.enter="reload"
      />
      <select v-model="kind" @change="reload">
        <option value="">all types</option>
        <option value="directory">directory</option>
        <option value="article">article</option>
        <option value="url-shortener">url-shortener</option>
        <option value="guestbook">guestbook</option>
        <option value="social-network">social-network</option>
        <option value="forum">forum</option>
        <option value="blog-comment">blog-comment</option>
        <option value="microblog">microblog</option>
        <option value="rss">rss</option>
        <option value="video">video</option>
        <option value="unknown">unknown</option>
      </select>
      <button @click="reload">Search</button>
    </div>

    <div class="card" style="padding:0;overflow:hidden">
      <table class="table">
        <thead>
          <tr>
            <th>Engine</th>
            <th>Type</th>
            <th class="spacer"></th>
            <th>Steps</th>
            <th>Category</th>
            <th>Source file</th>
          </tr>
        </thead>
        <tbody>
          <tr v-if="!store.items.length">
            <td colspan="6" class="muted" style="text-align:center;padding:28px">
              no engines match — try a different search/type
            </td>
          </tr>
          <tr v-for="e in store.items" :key="e.id">
            <td style="font-weight:600">{{ e.name }}</td>
            <td><span class="badge" :class="e.kind === 'directory' || e.kind === 'article' ? 'accent' : ''">{{ e.kind }}</span></td>
            <td></td>
            <td>{{ e.step_count }}</td>
            <td><span class="badge">{{ e.category }}</span></td>
            <td class="mono muted">{{ e.file_path }}</td>
          </tr>
        </tbody>
      </table>
      <div class="pager" style="padding:10px 14px;border-top:1px solid var(--border)">
        <button :disabled="!canPrev" @click="prev">← prev</button>
        <span class="muted" style="font-size:12px">
          page {{ Math.floor(offset / PER_PAGE) + 1 }} · total {{ store.total.toLocaleString() }}
        </span>
        <button :disabled="!canNext" @click="next">next →</button>
      </div>
    </div>

    <div v-if="store.error" class="error">{{ store.error }}</div>
  </div>
</template>
