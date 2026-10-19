import { createApp } from 'vue'
import { createPinia } from 'pinia'

import App from './App.vue'
import router from './router'
import './style.css'

// BUG-1 hardening: a silent component/render failure during route activation
// (stale tab state, interrupted lazy chunk, app-level exception) used to leave
// the URL changed but the old view rendered. Surface every such error and, as a
// guaranteed fallback, do a one-shot full reload on the current route — which
// always renders the correct view (verified: full loads work, only in-place
// activation can go stale).
router.onError((err, to) => {
  console.error('[router] navigation error', to?.fullPath, err)
  ;(window as any).__ser_nav_error = String(err)
})

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.config.errorHandler = (err, _instance, info) => {
  console.error('[app] component error', info, err)
  if (!(window as any).__ser_fatal_reloaded) {
    ;(window as any).__ser_fatal_reloaded = true
    console.warn('[app] recovering with full reload on', window.location.pathname)
    setTimeout(() => window.location.reload(), 120)
  }
}
app.mount('#app')
