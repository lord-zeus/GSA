<script setup lang="ts">
import { RouterView } from 'vue-router'
import { useHealthStore } from '@/stores/health'
import { onMounted, computed } from 'vue'

const health = useHealthStore()
onMounted(() => health.refresh())
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
    </main>
  </div>
</template>
