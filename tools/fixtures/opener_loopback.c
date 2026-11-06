/* Windows loopback launcher for the pinned OpENer sample adapter. */
#include <stdio.h>
#include "generic_networkhandler.h"
#include "opener_api.h"
#include "cipethernetlink.h"
#include "ciptcpipinterface.h"
#include "doublylinkedlist.h"
#include "cipconnectionobject.h"

extern EipUint8 g_assembly_data064[32];

static volatile LONG stop_requested;

static DWORD WINAPI wait_for_stop(LPVOID unused) {
  (void)unused;
  (void)getchar();
  InterlockedExchange(&stop_requested, 1);
  return 0;
}

int main(void) {
  DoublyLinkedListInitialize(&connection_list, CipConnectionObjectListArrayAllocator,
                             CipConnectionObjectListArrayFree);
  SetDeviceSerialNumber(123456789);
  CipStackInit(0x1122);
  uint8_t mac[6] = {2, 0, 0, 0, 0, 2};
  CipEthernetLinkSetMac(mac);
  g_tcpip.interface_configuration.ip_address = htonl(0x7f000002u);
  g_tcpip.interface_configuration.network_mask = htonl(0xff000000u);
  for (EipUint8 i = 0; i < sizeof(g_assembly_data064); ++i) {
    g_assembly_data064[i] = i;
  }

  if (kEipStatusOk != NetworkHandlerInitialize()) {
    ShutdownCipStack();
    return 1;
  }
  HANDLE stop_thread = CreateThread(NULL, 0, wait_for_stop, NULL, 0, NULL);
  if (stop_thread == NULL) {
    NetworkHandlerFinish();
    ShutdownCipStack();
    return 1;
  }

  puts("OpENer loopback ready");
  fflush(stdout);
  EipStatus status = kEipStatusOk;
  while (InterlockedCompareExchange(&stop_requested, 0, 0) == 0 &&
         status == kEipStatusOk) {
    status = NetworkHandlerProcessCyclic();
  }
  CloseHandle(stop_thread);
  NetworkHandlerFinish();
  ShutdownCipStack();
  return status == kEipStatusOk ? 0 : 1;
}
