// Start with: npx electron . --remote-debugging-port=9333
const { app, BrowserWindow } = require('electron');

app.whenReady().then(() => {
  const win = new BrowserWindow({ width: 640, height: 400, show: !process.env.KLYRO_HIDE_WINDOW });
  win.loadFile('index.html');
});
app.on('window-all-closed', () => app.quit());
