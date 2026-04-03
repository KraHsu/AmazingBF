// 求解:
//   a*x + b*y = e
//   c*x + d*y = f
//
// 解 (Cramer 法则):
//   det = a*d - b*c
//   x   = (e*d - b*f) / det
//   y   = (a*f - e*c) / det
//
// 限制: 所有中间值须在 0-255 范围内 (8位无符号运算)
//       确保 det != 0 且为正, 且 (e*d - b*f), (a*f - e*c) 均为正
//       且能被 det 整除 (否则结果为截断整数商)
//
// 示例输入 (每个数字后回车):
//   2       ← a
//   1       ← b
//   5       ← e
//   1       ← c
//   3       ← d
//   10      ← f
// 对应方程:  2x + y = 5,  x + 3y = 10
// 解: x = 1, y = 3
// 输出: x=1 y=3

int a;
int b;
int c;
int d;
int e;
int f;
int det;
int xn;
int yn;
int x;
int y;

void main() {
    // 读入系数
    scan(a);
    scan(b);
    scan(e);
    scan(c);
    scan(d);
    scan(f);

    // 计算行列式 det = a*d - b*c
    det = a * d - b * c;

    // 计算分子
    xn = e * d - b * f;
    yn = a * f - e * c;

    // 求解
    x = xn / det;
    y = yn / det;

    // 输出 "x="
    print_char(120);
    print_char(61);
    print(x);

    // 输出 "y="
    print_char(121);
    print_char(61);
    print(y);

    print_char(10);
}
