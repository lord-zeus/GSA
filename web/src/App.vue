<script setup lang="ts">
import { RouterView } from 'vue-router'
import { useHealthStore } from '@/stores/health'
import { useNotificationStore } from '@/stores/notifications'
import { onMounted, onBeforeUnmount, computed } from 'vue'

const health = useHealthStore()
const notes = useNotificationStore()
onMounted(() => {
  health.refresh()
  // F#18: one app-wide SSE subscription; the bell, toasts and store all read
  // the same Pinia state, and the list re-fetches on every (re)connect.
  notes.connect()
})
onBeforeUnmount(() => notes.disconnect())
const ok = computed(() => health.status === 'ok' && health.db === 'up')
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="logo">S</div>
        <div>
          <div class="name">SER Replica</div>
          <div class="sub">ranker dashboard</div>
        </div>
      </div>
      <nav class="nav">
        <router-link to="/" exact-active-class="active">
          <span>◈</span> Overview
        </router-link>
        <router-link to="/engines" active-class="active">
          <span>⚙</span> Engines
        </router-link>
        <router-link to="/projects" active-class="active">
          <span>▣</span> Projects
        </router-link>
        <router-link to="/proxies" active-class="active">
          <span>⇌</span> Proxies
        </router-link>
        <router-link to="/stats" active-class="active">
          <span>📊</span> Stats
        </router-link>
        <router-link to="/schedules" active-class="active">
          <span>⏰</span> Schedules
        </router-link>
        <router-link to="/notifications" active-class="active">
          <span>🔔</span>
          <span>Notifications</span>
          <span v-if="notes.unread > 0" class="bell-badge">{{ notes.unread > 99 ? '99+' : notes.unread }}</span>
        </router-link>
        <router-link to="/settings" active-class="active">
          <span>⚙︎</span> Settings
        </router-link>
      </nav>
      <div class="side-foot">
        <span class="dot" :class="ok ? 'ok' : 'bad'"></span>&nbsp;
        <span v-if="ok">API + DB online</span>
        <span v-else-if="health.status === 'degraded'">API up · DB down</span>
        <span v-else>API unreachable</span>
      </div>
    </aside>
    <main class="main">
      <RouterView />
      <!-- F#18: real-time toasts, right side -->
      <div class="toasts" aria-live="polite">
        <div v-for="t in notes.toasts" :key="t.id" class="toast" :class="t.severity">
          <div class="toast-head">
            <span class="toast-sev">{{ t.severity === 'good' ? '✓' : t.severity === 'warn' ? '!' : t.severity === 'bad' ? '✕' : '·' }}</span>
            <span class="toast-title">{{ t.title }}</span>
            <button class="toast-x" title="dismiss" @click="notes.dismissToast(t.id)">×</button>
          </div>
          <div class="toast-msg">{{ t.message }}</div>
        </div>
      </div>
    </main>
  </div>
</template>

<style scoped>
.bell-badge {
  margin-left: auto;
  background: #e25959;
  color: #fff;
  font-size: 11px;
  font-weight: 700;
  line-height: 16px;
  min-width: 16px;
  height: 16px;
  border-radius: 9px;
  text-align: center;
  padding: 0 4px;
}
.toasts {
  position: fixed;
  right: 16px;
  top: 16px;
  z-index: 50;
  display: flex;
  flex-direction: column;
  gap: 10px;
  width: 340px;
  max-width: calc(100vw - 32px);
  pointer-events: none;
}
.toast {
  pointer-events: auto;
  border: 1px solid var(--border, #333);
  border-left-width: 4px;
  background: var(--card, #1b1b1f);
  color: var(--fg, #eee);
  border-radius: 8px;
  padding: 10px 12px;
  box-shadow: 0 6px 24px rgba(0, 0, 0, .35);
  animation: toast-in .18s ease-out;
}
@keyframes toast-in {
  from { opacity: 0; transform: translateX(12px); }
  to { opacity: 1; transform: none; }
}
.toast.good { border-left-color: #3fb950; }
.toast.warn { border-left-color: #d9a514; }
.toast.bad { border-left-color: #e25959; }
.toast.info { border-left-color: #4a8fe2; }
.toast-head { display: flex; align-items: center; gap: 8px; }
.toast.good .toast-sev { color: #3fb950; }
.toast.warn .toast-sev { color: #d9a514; }
.toast.bad .toast-sev { color: #e25959; }
.toast.info .toast-sev { color: #4a8fe2; }
.toast-title { font-weight: 600; font-size: 13px; flex: 1; }
.toast-x {
  background: none;
  border: none;
  color: inherit;
  opacity: .55;
  cursor: pointer;
  font-size: 14px;
  padding: 0 2px;
}
.toast-x:hover { opacity: 1; }
.toast-msg { font-size: 12px; opacity: .85; margin-top: 4px; line-height: 1.45; word-break: break-word; }
</style>
