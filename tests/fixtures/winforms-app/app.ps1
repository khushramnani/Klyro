# Tiny native Windows app for Klyro's desktop tests (WinForms; ships with Windows PowerShell).
#   powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File app.ps1
Add-Type -AssemblyName System.Windows.Forms

$form = New-Object Windows.Forms.Form
$form.Text = 'Klyro WinForms sample'
$form.Width = 420
$form.Height = 220

$label = New-Object Windows.Forms.Label
$label.Text = 'Your name'
$label.Location = New-Object Drawing.Point(12, 15)
$label.AutoSize = $true

$box = New-Object Windows.Forms.TextBox
$box.Location = New-Object Drawing.Point(100, 12)
$box.Width = 200
$box.AccessibleName = 'Your name'

$loud = New-Object Windows.Forms.CheckBox
$loud.Text = 'Loud'
$loud.Location = New-Object Drawing.Point(200, 45)

$button = New-Object Windows.Forms.Button
$button.Text = 'Greet'
$button.Location = New-Object Drawing.Point(100, 45)

$out = New-Object Windows.Forms.Label
$out.Text = 'Nobody greeted yet'
$out.Location = New-Object Drawing.Point(12, 90)
$out.AutoSize = $true

$button.Add_Click({
    $t = "Hello, $($box.Text)"
    if ($loud.Checked) { $t = $t.ToUpper() + '!' }
    $out.Text = $t
})

$form.Controls.AddRange(@($label, $box, $loud, $button, $out))
[void]$form.ShowDialog()
