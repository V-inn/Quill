/* Milestone 1 spike: connect to an evdi card, supply a real EDID, and log
 * mode_changed / update_ready events to prove userspace can read raw
 * framebuffer updates. Throwaway — not part of the real daemon. */

#include <evdi_lib.h>

#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static volatile sig_atomic_t g_stop;
static evdi_handle g_handle;
static int g_buffer_id = 1;
static struct evdi_buffer g_buffer;
static int g_have_buffer;

static void on_sigint(int sig) {
	(void)sig;
	g_stop = 1;
}

static void mode_changed_handler(struct evdi_mode mode, void *user_data) {
	(void)user_data;
	printf("[mode_changed] %dx%d @%dHz bpp=%d pixel_format=0x%x\n",
	       mode.width, mode.height, mode.refresh_rate,
	       mode.bits_per_pixel, mode.pixel_format);

	if (g_have_buffer) {
		evdi_unregister_buffer(g_handle, g_buffer_id);
		free(g_buffer.buffer);
		free(g_buffer.rects);
		g_have_buffer = 0;
	}

	int stride = mode.width * 4;
	g_buffer.id = g_buffer_id;
	g_buffer.width = mode.width;
	g_buffer.height = mode.height;
	g_buffer.stride = stride;
	g_buffer.buffer = calloc(1, (size_t)stride * (size_t)mode.height);
	g_buffer.rect_count = 16;
	g_buffer.rects = calloc((size_t)g_buffer.rect_count, sizeof(struct evdi_rect));

	if (!g_buffer.buffer || !g_buffer.rects) {
		fprintf(stderr, "allocation failed for %dx%d framebuffer\n",
			mode.width, mode.height);
		exit(1);
	}

	evdi_register_buffer(g_handle, g_buffer);
	g_have_buffer = 1;
	evdi_request_update(g_handle, g_buffer_id);
}

static void update_ready_handler(int buffer_to_be_updated, void *user_data) {
	(void)user_data;
	int num_rects = g_buffer.rect_count;

	evdi_grab_pixels(g_handle, g_buffer.rects, &num_rects);
	printf("[update_ready] buffer=%d dirty_rects=%d\n", buffer_to_be_updated,
	       num_rects);

	evdi_request_update(g_handle, g_buffer_id);
}

static void dpms_handler(int dpms_mode, void *user_data) {
	(void)user_data;
	printf("[dpms] mode=%d\n", dpms_mode);
}

static void crtc_state_handler(int state, void *user_data) {
	(void)user_data;
	printf("[crtc_state] state=%d\n", state);
}

int main(int argc, char **argv) {
	if (argc != 2) {
		fprintf(stderr, "usage: %s <card-number>\n", argv[0]);
		return 1;
	}
	int card = atoi(argv[1]);

	enum evdi_device_status status = evdi_check_device(card);
	if (status != AVAILABLE) {
		fprintf(stderr, "card%d is not an available evdi device (status=%d)\n",
			card, status);
		return 1;
	}

	g_handle = evdi_open(card);
	if (g_handle == EVDI_INVALID_HANDLE) {
		fprintf(stderr, "evdi_open(%d) failed\n", card);
		return 1;
	}

	FILE *f = fopen("/sys/class/drm/card0-eDP-1/edid", "rb");
	if (!f) {
		fprintf(stderr, "failed to open eDP EDID: %s\n", strerror(errno));
		return 1;
	}
	unsigned char edid[128];
	size_t n = fread(edid, 1, sizeof(edid), f);
	fclose(f);
	if (n != sizeof(edid)) {
		fprintf(stderr, "unexpected EDID size: %zu bytes\n", n);
		return 1;
	}

	evdi_connect(g_handle, edid, (unsigned int)sizeof(edid), 0);
	printf("connected to card%d, waiting for mode change...\n", card);

	signal(SIGINT, on_sigint);

	struct evdi_event_context ctx = {
		.dpms_handler = dpms_handler,
		.mode_changed_handler = mode_changed_handler,
		.update_ready_handler = update_ready_handler,
		.crtc_state_handler = crtc_state_handler,
	};

	evdi_selectable fd = evdi_get_event_ready(g_handle);
	struct pollfd pfd = { .fd = fd, .events = POLLIN };

	while (!g_stop) {
		int rc = poll(&pfd, 1, 1000);
		if (rc < 0) {
			if (errno == EINTR)
				continue;
			perror("poll");
			break;
		}
		if (rc > 0 && (pfd.revents & POLLIN))
			evdi_handle_events(g_handle, &ctx);
	}

	printf("shutting down...\n");
	if (g_have_buffer) {
		evdi_unregister_buffer(g_handle, g_buffer_id);
		free(g_buffer.buffer);
		free(g_buffer.rects);
	}
	evdi_disconnect(g_handle);
	evdi_close(g_handle);
	return 0;
}
