import Foundation
import OpenUsage
#if canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

@main
struct OpenUsageLinuxMain {
    static func main() async {
        let code = await OpenUsageLinuxCommand.main(Array(CommandLine.arguments.dropFirst()))
        exit(code)
    }
}
