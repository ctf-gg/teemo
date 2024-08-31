// struct Thing {
//     char a;
//     long long b;
//     double *c;
//     char d[4];
// };

typedef int meow;
int(*ttt)(int, char) = 0;

enum a {
    hi
};

// int something(int nothing) {
//     int other = nothing;
// }

struct Thing {
    union {
        int a;
        char b;
    } c[2];
    void *d;
} thing;

int main(int argc) {

}