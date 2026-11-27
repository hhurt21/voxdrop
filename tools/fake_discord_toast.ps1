param([string]$Mode = "show", [string]$Body = "テスト本文です")
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
$app = "com.squirrel.Discord.Discord"
$tag = "voxdrop-probe"
$grp = "Notifications"
if ($Mode -eq "remove") {
    [Windows.UI.Notifications.ToastNotificationManager]::History.Remove($tag, $grp, $app)
    "removed"
    exit
}
$iso1 = [char]0x2068
$iso2 = [char]0x2069
$title = "${iso1}VoxDropテスト${iso2}"
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument
$xml.LoadXml("<toast><visual><binding template=`"ToastGeneric`"><text>$title</text><text>$Body</text></binding></visual><audio silent=`"true`"/></toast>")
$t = New-Object Windows.UI.Notifications.ToastNotification $xml
$t.Tag = $tag
$t.Group = $grp
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show($t)
"shown"
