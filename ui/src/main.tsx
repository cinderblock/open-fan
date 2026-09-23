import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import { installExternalLinks } from './external';
import './styles.css';

const root = document.getElementById('root');
if (!root) throw new Error('missing #root');

// Before rendering, so no link can be clicked before it is handled. Never uninstalled:
// it lives as long as the document does.
installExternalLinks();

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
