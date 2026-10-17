import { createRouter, createWebHistory } from 'vue-router'

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes: [
    { path: '/', name: 'overview', component: () => import('@/views/DashboardView.vue') },
    { path: '/engines', name: 'engines', component: () => import('@/views/EnginesView.vue') },
    { path: '/projects', name: 'projects', component: () => import('@/views/ProjectsView.vue') },
    { path: '/proxies', name: 'proxies', component: () => import('@/views/ProxiesView.vue') },
  ],
})

export default router
