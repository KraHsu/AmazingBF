// 输入: 第一行 n (元素个数, 最大 10)
//       后续 n 行, 每行一个 0-255 的整数
// 输出: 排序后的数字, 空格分隔
//
// 示例输入 (每个数字后回车):
//   5
//   3
//   1
//   4
//   1
//   5
// 示例输出:
//   1 1 3 4 5

int n;
int arr[10];
int i;
int j;
int bound;
int t;

void main() {
    // 读入 n
    scan(n);

    // 读入数组
    i = 0;
    while (i < n) {
        scan(arr[i]);
        i = i + 1;
    }

    // 冒泡排序
    i = 0;
    while (i < n) {
        j = 0;
        bound = n - i - 1;
        while (j < bound) {
            // 比较 arr[j] 和 arr[j+1]
            t = j + 1;
            if (arr[j] > arr[t]) {
                // 交换
                t = arr[j];
                arr[j] = arr[j + 1];
                arr[j + 1] = t;
            }
            j = j + 1;
        }
        i = i + 1;
    }

    // 输出
    i = 0;
    while (i < n) {
        print(arr[i]);
        i = i + 1;
    }
    print_char(10);
}
