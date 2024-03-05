const express = require('express');
const bodyParser = require('body-parser');
const mongoose = require('mongoose');
const { transferRouter } = require('./routes/transfer');
const { swapRouter } = require('./routes/swap');
const swaggerUi = require('swagger-ui-express');
const swaggerDocument = require('./swagger.json');
const app = express();
app.use(bodyParser.json());
app.use(transferRouter);
app.use(swapRouter);
// Serve Swagger UI from the /api-docs endpoint
app.use('/api-docs', swaggerUi.serve, swaggerUi.setup(swaggerDocument));

mongoose.connect('mongodb://localhost:27017/trading', {
  useNewUrlParser: true,
  useUnifiedTopology: true
}).then(() => {
  console.log('Connected to the database');
}).catch((error:any) => {
  console.error('Error connecting to the database', error);
});

app.listen(5000, () => {
    console.log('server is listening on port 5000');
});
