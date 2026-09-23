import React from 'react'
import ReactDOM from 'react-dom/client'
import { getCurrentWindow } from '@tauri-apps/api/window'
import App from './App'
import { TrayPanel } from './views/TrayPanel'
import './styles.css'

// Both windows load the same bundle; the label decides which one this is.
const isTray = getCurrentWindow().label === 'tray'
if (isTray) document.documentElement.classList.add('is-tray')

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>{isTray ? <TrayPanel /> : <App />}</React.StrictMode>,
)
