// Parity fixture: small file with deliberate bugs that exercise several
// different cppcheck checkers (and the std.cfg library data for memleak/
// nullPointer on malloc/free knowledge).
#include <cstdlib>
#include <cstring>
#include <vector>

int uninitialized_read()
{
    int x;          // uninitvar
    return x + 1;
}

void memory_leak()
{
    char *p = static_cast<char*>(std::malloc(64));   // memleak (needs std.cfg)
    std::strcpy(p, "hi");                            // nullPointerOutOfMemory
}   // leak: p never freed

int out_of_bounds()
{
    int arr[4] = {0, 1, 2, 3};
    return arr[4]; // arrayIndexOutOfBounds
}

int null_deref(bool flag)
{
    int *q = nullptr;
    if (flag)
        return *q; // nullPointer
    return 0;
}

int division_by_zero()
{
    int zero = 0;
    return 100 / zero; // zerodiv
}

void vector_oob()
{
    std::vector<int> v;
    v.push_back(1);
    (void)v[3]; // containerOutOfBounds / stlOutOfBounds
}
