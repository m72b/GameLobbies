import './lobby-api'
import React from 'react'
import { createRoot } from 'react-dom/client'
import App from './App'
import './styles.css'

// index.html runs /freeze-prototype.js before any app code (it replaces Tauri's app-wide
// freezePrototype). If it didn't load, say so in the console and keep going: the app still
// works, only that hardening is missing.
if (!Object.isFrozen(Object.prototype)) console.warn('GameLobbies: Object.prototype is not frozen; /freeze-prototype.js did not run.')

createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>)
