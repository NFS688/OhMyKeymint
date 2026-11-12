<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { MiuixButton, MiuixCard, MiuixProgressIndicator,
  MiuixRadioButtonPreference, MiuixSwitchPreference } from 'miuix-vue'
import MiuixBottomSheet from './StableBottomSheet.vue'
import { type Cli, type KeyboxAutoSettings } from '../cli'
import { afterPaint } from '../app_list/app_list'
import { i18n } from '../i18n'
import { isDev } from '../utils/dev'

const props = defineProps<{ modelValue: boolean, cli: Cli }>()
const emit = defineEmits<{
  'update:modelValue': [open: boolean]
  notify: [message: string, error?: boolean]
  changed: []
}>()
const settings = ref<KeyboxAutoSettings | null>(null)
const interval = ref('60')
const busy = ref(false)
const error = ref('')
let generation = 0
const validInterval = computed(() => /^\d+$/.test(interval.value)
  && Number(interval.value) >= 1 && Number(interval.value) <= 43200)

async function load(): Promise<void> {
  const current = ++generation
  settings.value = null
  error.value = ''
  try {
    await afterPaint()
    const value: KeyboxAutoSettings = isDev()
      ? { enabled: false, source: 'yurikey', interval_minutes: 60, proxy_prefix: '' }
      : await props.cli.getKeyboxAuto()
    if (current !== generation || !props.modelValue) return
    settings.value = value
    interval.value = String(value.interval_minutes)
  } catch (reason) {
    if (current === generation && props.modelValue) error.value = String(reason)
  }
}

watch(() => props.modelValue, open => { if (open) void load(); else generation++ })
onBeforeUnmount(() => { generation++ })

function requestClose(): boolean {
  if (busy.value) return false
  emit('update:modelValue', false)
  return true
}
defineExpose({ requestClose, busy })

async function apply(fetchNow: boolean): Promise<void> {
  if (busy.value || !settings.value || !validInterval.value) return
  busy.value = true
  error.value = ''
  try {
    const value = { ...settings.value, interval_minutes: Number(interval.value),
      proxy_prefix: settings.value.proxy_prefix.trim() }
    await afterPaint()
    if (isDev()) {
      emit('notify', i18n.t('keybox_auto_preview'))
      return
    }
    settings.value = await props.cli.updateKeyboxAuto(value, fetchNow)
    emit('notify', i18n.t(fetchNow ? 'keybox_auto_fetched' : 'keybox_auto_saved'))
    if (fetchNow) emit('changed')
  } catch (reason) {
    error.value = reason instanceof Error ? reason.message : String(reason)
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <MiuixBottomSheet :model-value="modelValue" :title="i18n.t('keybox_auto_title')"
    :allow-dismiss="!busy" :close-on-click-modal="!busy"
    @update:model-value="value => value ? undefined : requestClose()" @close="requestClose">
    <div class="keybox-auto-sheet" :aria-busy="busy">
      <p class="keybox-auto-note">{{ i18n.t('keybox_auto_notice') }}</p>
      <template v-if="settings">
        <MiuixCard press-feedback="none">
          <MiuixSwitchPreference v-model="settings.enabled" :disabled="busy"
            :title="i18n.t('keybox_auto_enable')" :summary="i18n.t('keybox_auto_background')" />
        </MiuixCard>
        <MiuixCard press-feedback="none">
          <MiuixRadioButtonPreference :model-value="settings.source === 'yurikey'" :disabled="busy"
            title="Yurikey" @select="settings.source = 'yurikey'" />
          <MiuixRadioButtonPreference :model-value="settings.source === 'integrity_box'" :disabled="busy"
            title="IntegrityBox" @select="settings.source = 'integrity_box'" />
        </MiuixCard>
        <MiuixCard class="keybox-auto-fields" press-feedback="none">
          <label>
            <span>{{ i18n.t('keybox_auto_interval') }}</span>
            <input v-model="interval" type="number" min="1" max="43200" step="1" inputmode="numeric"
              :disabled="busy" :aria-invalid="!validInterval">
            <small v-if="!validInterval" role="alert">{{ i18n.t('keybox_auto_interval_error') }}</small>
          </label>
          <label>
            <span>{{ i18n.t('keybox_auto_proxy') }}</span>
            <input v-model="settings.proxy_prefix" type="url" :disabled="busy" maxlength="2048"
              spellcheck="false" autocapitalize="none" autocomplete="off"
              :placeholder="i18n.t('keybox_auto_proxy_placeholder')">
            <small>{{ i18n.t('keybox_auto_proxy_help') }}</small>
          </label>
        </MiuixCard>
        <div class="keybox-auto-actions">
          <MiuixButton :disabled="busy || !validInterval" @click="apply(false)">
            {{ i18n.t('functional_button_save') }}
          </MiuixButton>
          <MiuixButton type="primary" :disabled="busy || !validInterval" @click="apply(true)">
            {{ i18n.t('keybox_auto_fetch') }}
          </MiuixButton>
        </div>
      </template>
      <MiuixProgressIndicator v-if="busy || (!settings && !error)" type="circular" :size="26" />
      <p v-if="error" role="alert" class="keybox-auto-error">{{ error }}</p>
      <MiuixButton v-if="error && !settings" @click="load">{{ i18n.t('functional_button_retry') }}</MiuixButton>
    </div>
  </MiuixBottomSheet>
</template>

<style scoped>
.keybox-auto-sheet {
  display: flex;
  flex-direction: column;
  gap: 12px;
  min-height: min(50vh, 420px);
  padding: 2px 0 max(18px, env(safe-area-inset-bottom));
}
.keybox-auto-sheet > * { flex: none; }
.keybox-auto-note { margin: 0 16px; color: var(--m-color-on-surface-variant-summary); font-size: var(--m-text-body2-size); line-height: 1.5; }
.keybox-auto-fields label { display: grid; gap: 8px; padding: 16px; }
.keybox-auto-fields input {
  min-width: 0;
  width: 100%;
  box-sizing: border-box;
  border: 1px solid var(--m-color-outline);
  border-radius: var(--m-radius-md);
  padding: 12px;
  color: var(--m-color-on-surface);
  background: var(--m-color-surface-container);
  font: inherit;
}
.keybox-auto-fields input:focus-visible { outline: 2px solid var(--m-color-primary); }
.keybox-auto-fields small { color: var(--m-color-on-surface-variant-summary); line-height: 1.5; overflow-wrap: anywhere; }
.keybox-auto-fields input[aria-invalid="true"], .keybox-auto-error { color: var(--m-color-error); }
.keybox-auto-error { margin: 0 16px; overflow-wrap: anywhere; }
.keybox-auto-actions { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
</style>
