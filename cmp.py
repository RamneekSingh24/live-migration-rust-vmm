f1 = open('mem_snap.txt', 'rb')
f2 = open('mem_live.txt', 'rb')

data1 = f1.read()
data2 = f2.read()

if data1 == data2:
    print("OK")
else:
    print("NOT OK")