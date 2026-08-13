CFLAGS := -Wall -Wextra -O2
LDLIBS := -levdi

evdi_test_client: evdi_test_client.c
	$(CC) $(CFLAGS) -o $@ $< $(LDLIBS)

clean:
	rm -f evdi_test_client

.PHONY: clean
