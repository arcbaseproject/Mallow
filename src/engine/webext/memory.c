/* WebKit web-process extension: answers "mallow-memory" with this web
 * process's proportional set size in bytes, so the UI can show per-tab RAM.
 * Web processes run in their own PID namespace, so the UI can't read their
 * /proc entries itself. Built by build.rs and loaded from Mallow's cache dir. */
#include <stdio.h>
#include <webkit/webkit-web-process-extension.h>

static gboolean on_message(WebKitWebPage *page, WebKitUserMessage *msg, gpointer data) {
    (void)page;
    (void)data;
    if (g_strcmp0(webkit_user_message_get_name(msg), "mallow-memory") != 0)
        return FALSE;
    guint64 kb = 0;
    FILE *f = fopen("/proc/self/smaps_rollup", "r");
    if (f) {
        char line[256];
        while (fgets(line, sizeof line, f))
            if (sscanf(line, "Pss: %" G_GUINT64_FORMAT, &kb) == 1)
                break;
        fclose(f);
    }
    webkit_user_message_send_reply(msg, webkit_user_message_new("mallow-memory", g_variant_new_uint64(kb * 1024)));
    return TRUE;
}

static void on_page(WebKitWebProcessExtension *ext, WebKitWebPage *page, gpointer data) {
    (void)ext;
    (void)data;
    g_signal_connect(page, "user-message-received", G_CALLBACK(on_message), NULL);
}

G_MODULE_EXPORT void webkit_web_process_extension_initialize(WebKitWebProcessExtension *ext) {
    g_signal_connect(ext, "page-created", G_CALLBACK(on_page), NULL);
}
